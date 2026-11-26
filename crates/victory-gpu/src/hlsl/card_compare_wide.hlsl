#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), UAV(u0), UAV(u1)"
cbuffer Span : register(b0) {
    uint first;    // the first piece
    uint skew;     // where the copy starts in its piece 0
    uint size;     // the copy's buffer, in bytes (a multiple of 64 KiB)
    uint pieces;
    uint out_at;   // the first piece's dword in `differs`, in bytes
    uint compare;  // 0: the hashes are stored; else compared with those stored (2: and then stored)
};
ByteAddressBuffer now : register(t0);
// 16 bytes per piece of the copy: the hash of its bytes when they were
// last sent up (or hashed whole, the first time).
RWByteAddressBuffer hashes : register(u0);
RWByteAddressBuffer differs : register(u1);
// A group a piece; its threads take every 64th 16 bytes each, so that a pass
// over few pieces still fills the card. The parts' seeds differ, and each ends
// in a mix, so parts that trade places do not cancel out.
groupshared uint4 part[64];
uint4 mix(uint4 h, uint4 v) {
    h = (h ^ v) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
    return h.yzwx ^ (h >> 15);
}
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 group : SV_GroupID, uint t : SV_GroupIndex) {
    uint piece = first + group.x;
    uint lo = piece * 65536u;
    lo = lo > skew ? lo - skew : 0u;
    uint hi = min((piece + 1u) * 65536u - skew, size);
    lo = lo & ~15u;
    hi = (hi + 15u) & ~15u;
    uint4 h = uint4(0x9E3779B9u, 0x85EBCA6Bu, 0xC2B2AE35u, 0x27D4EB2Fu) * (2u * t + 1u);
    [loop]
    for (uint at = lo + t * 16u; at < hi; at += 1024u) {
        h = mix(h, now.Load4(at));
    }
    part[t] = mix(mix(h, t.xxxx), h.wxyz);
    GroupMemoryBarrierWithGroupSync();
    if (t != 0u) return;
    uint4 all_parts = uint4(0u, 0u, 0u, 0u);
    for (uint i = 0u; i < 64u; i++) {
        all_parts += part[i];
    }
    if (compare != 0u) {
        differs.Store(out_at + group.x * 4u, any(all_parts != hashes.Load4(piece * 16u)) ? 1u : 0u);
        // With the copy-back on its own thread: the next compare of the
        // piece finds what is written from here on.
        if (compare == 2u) hashes.Store4(piece * 16u, all_parts);
    } else {
        hashes.Store4(piece * 16u, all_parts);
    }
}
