#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Record : register(b0) {
    uint kind;
    uint stride;
    uint records;
    uint args_offset;
    // Bit 0: a count buffer bounds the records. Bit 1: the records are a
    // window clamped ahead of their draws: one that asks for more than can
    // run is left empty and costs nothing. Bit 2: the records are gathered
    // (`gather`): record i lies in the source where word i of the table
    // behind `count_buf` says (from `count_offset` on), and goes to slot i.
    // Bit 3: dispatch records spread across z past 65,535 groups in x
    // (`crate::draw`'s spread command signature): record i goes to byte
    // 20 * i as [row, asked x, x, y, z], row 0 when not spread.
    uint has_count;
    uint count_offset;
};
ByteAddressBuffer src_args : register(t0);
ByteAddressBuffer count_buf : register(t1);
RWByteAddressBuffer dst_args : register(u0);
// The list's work so far (vertex instances, threads): past the budget,
// records run empty.
RWByteAddressBuffer budget : register(u1);
static const uint BUDGET = 1u << 27;
// The vertices or indices an instance from which a draw record runs empty.
static const uint MOST_DRAW = 1u << 23;
// Groups of one dispatch; more run empty.
static const uint GROUPS = 1u << 20;
// Each group charges the budget once: the costs of its records go here,
// one thread charges their sum, saturating as each record would in lane
// order, and every thread then reads whether its own record fitted. A
// compare and swap per record, 64 threads racing for one word, cost a
// round for each record of a window, and in memory the CPU maps (where the
// budget was, and is with VICTORY_INDIRECT_CARD_BUDGET=0) every atomic
// crosses the bus on a card of its own.
groupshared uint costs[64];
groupshared uint fits[64];
groupshared uint largest_asked;
groupshared uint all_asked;
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID, uint gi : SV_GroupIndex) {
    uint eff_count = records;
    bool ahead = (has_count & 2u) != 0;
    if ((has_count & 1u) != 0) {
        eff_count = min(eff_count, count_buf.Load(count_offset));
    }
    // Threads past the records take part in the group's charge (cost 0)
    // and store nothing.
    bool active = id.x < eff_count;
    uint src_at = args_offset + id.x * stride;
    // (A branch, not a select: without the bit the view behind `count_buf`
    // is a count of four bytes, or the source itself, and is not read here.)
    [branch]
    if ((has_count & 4u) != 0) {
        src_at = count_buf.Load(count_offset + id.x * 4u);
    }
    uint dst_at = id.x * stride;
    bool spread = (has_count & 8u) != 0;
    uint row = 0;
    uint asked_x = 0;
    if (spread) {
        dst_at = id.x * 20u;
    }
    uint cost = 0;
    uint asked = 0;
    uint2 draw_counts = uint2(0, 0);
    uint3 groups = uint3(0, 0, 0);
    if (gi == 0) {
        largest_asked = 0;
        all_asked = 0;
    }
    if (active && kind <= 1) {
        // Draws: vertex or index count, then instances.
        uint2 want = src_args.Load2(src_at);
        if (ahead && (want.x > (1u << 24) || want.y > (1u << 24))) {
            want = uint2(0, 0);
        }
        uint count = min(want.x, 1u << 24);
        uint instances = min(want.y, (1u << 26) / max(count, 1u));
        cost = count * instances;
        // A record that asks for MOST_DRAW vertices or indices an instance
        // or more is no draw of the title's (no mesh of a console title has
        // 2.8 million triangles): it runs empty and charges nothing. Run
        // 206's window check found one list with 90 draws of 2^24 vertices
        // and one instance each. Clamped one draw at a time, the list's
        // budget lets eight of them run (2^27 in all) and empties the other
        // 82; served from a window, whose records are charged once however
        // often they are drawn, all 90 ran: 1.5 thousand million vertices
        // in one list, with windows on in the runs that lost the device
        // (201, 204).
        if (count >= MOST_DRAW) {
            count = 0;
            instances = 0;
            cost = 0;
        }
        draw_counts = uint2(count, instances);
    } else if (active && kind == 2) {
        uint3 raw = src_args.Load3(src_at);
        // Diagnostics: what the record asked for before the clamp (one
        // dispatch runs at most 65,535 groups a dimension; run 200 saw
        // compute records ask 77,000 to 98,000 in x). A record ahead of
        // its dispatch the title has not filled is not counted.
        asked = max(raw.x, max(raw.y, raw.z));
        if (ahead && asked > (1u << 24)) {
            asked = 0;
        }
        uint3 g;
        asked_x = raw.x;
        // Past 65,535 groups in x (one row): spread across z, the program
        // takes (x + z * row, y, 0) and leaves past `asked_x` (after
        // upstream VIctory pull request #2312).
        if (spread && raw.x > 65535u && raw.y >= 1u && raw.y <= 65535u && raw.z == 1u
            && !(ahead && raw.x > (1u << 24))) {
            uint rows = (raw.x + 65534u) / 65535u;
            row = (raw.x + rows - 1u) / rows;
            g = uint3(row, raw.y, rows);
        } else {
            g = min(raw, uint3(65535u, 65535u, 65535u));
        }
        // x * y fits 32 bits; the whole count may not.
        if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
            g = uint3(0, 0, 0);
        }
        cost = g.x * g.y * g.z * 64u;
        groups = g;
    } else if (active) {
        // Mesh dispatches: the console's are one-dimensional.
        uint3 g = src_args.Load3(src_at);
        // Ahead of its draw, a record over the most a dispatch runs is a
        // slot the title has not filled (the title leaves all
        // ones there): empty, and not counted below either.
        if (ahead && g.x > 65535u) {
            g = uint3(0, 0, 0);
        }
        asked = g.x;
        g = uint3(min(g.x, 65535u), min(g.y, 1u), min(g.z, 1u));
        cost = g.x * g.y * g.z * 128u;
        groups = g;
    }
    costs[gi] = cost;
    GroupMemoryBarrierWithGroupSync();
    if (active && kind >= 2) {
        // Diagnostics, in the budget buffer's next words (below): the
        // largest group count a mesh or compute record asked for, and the
        // groups asked in all.
        uint was;
        InterlockedMax(largest_asked, asked, was);
        InterlockedAdd(all_asked, min(asked, 1u << 24), was);
    }
    // The group's own records: none for a group past a count buffer's
    // count (an execution may ask for thousands and count a few), which
    // touches the budget not at all, as its threads did not before.
    uint first = id.x - gi;
    uint n = eff_count > first ? min(64u, eff_count - first) : 0u;
    if (gi == 0 && n > 0) {
        // What the records charge, from `before` on, in lane order: a
        // record that does not fit whole runs empty and takes the rest.
        // Only the group's own records are walked (one, clamped alone).
        uint before;
        budget.InterlockedOr(0, 0, before);
        [allow_uav_condition]
        while (before < BUDGET) {
            uint next = before;
            for (uint j = 0; j < n; j++) {
                uint c = costs[j];
                if (c > 0 && next < BUDGET) {
                    next = (BUDGET - next <= c) ? BUDGET : (next + c);
                }
            }
            if (next == before) break;
            uint prev;
            budget.InterlockedCompareExchange(0, before, next, prev);
            if (prev == before) break;
            before = prev;
        }
        uint at = before;
        for (uint k = 0; k < n; k++) {
            uint c = costs[k];
            uint fit = 1;
            if (c > 0) {
                fit = (at < BUDGET && BUDGET - at >= c) ? 1u : 0u;
                at = (at >= BUDGET || BUDGET - at <= c) ? BUDGET : (at + c);
            }
            fits[k] = fit;
        }
    }
    GroupMemoryBarrierWithGroupSync();
    if (gi == 0 && kind >= 2 && n > 0) {
        uint was;
        budget.InterlockedMax(4, largest_asked, was);
        budget.InterlockedAdd(8, all_asked, was);
    }
    if (!active) {
        return;
    }
    bool over = fits[gi] == 0;
    if (kind <= 1) {
        if (over) {
            draw_counts = uint2(0, 0);
        }
        dst_args.Store2(dst_at, draw_counts);
        for (uint off = 8; off < stride; off += 4) {
            dst_args.Store(dst_at + off, src_args.Load(src_at + off));
        }
    } else {
        if (over) {
            groups = uint3(0, 0, 0);
        }
        if (spread) {
            dst_args.Store2(dst_at, uint2(row, asked_x));
            dst_args.Store3(dst_at + 8u, groups);
        } else {
            dst_args.Store3(dst_at, groups);
            for (uint off = 12; off < stride; off += 4) {
                dst_args.Store(dst_at + off, src_args.Load(src_at + off));
            }
        }
    }
}
