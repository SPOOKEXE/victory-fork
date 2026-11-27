//! The counters `native/vkd3d-victory-stats.patch` adds to vkd3d-proton's
//! memory allocator (its `libs/vkd3d/victory_stats.h` names the same
//! indices), read through the `d3d12core.dll` export
//! `vkd3d_victory_memory_stats`, and laid out as rows. Shared by the
//! overlay's vkd3d view (F2, `victory-gpu`) and the allocator benchmark
//! (`apps/vkd3d-bench`).

/// The export's indices (`enum vkd3d_victory_stat`).
pub mod at {
    pub const ALLOC_CALLS: usize = 1;
    pub const ALLOC_NS: usize = 2;
    pub const ALLOC_NS_MAX: usize = 3;
    pub const ALLOC_FAILS: usize = 4;
    pub const ALLOC_LATER_TYPE: usize = 5;
    pub const ALLOC_OFF_CARD: usize = 6;
    pub const ALLOC_OFF_CARD_BYTES: usize = 7;
    pub const FREE_CALLS: usize = 8;
    pub const FREE_NS: usize = 9;
    pub const SUB_CALLS: usize = 10;
    pub const SUB_NS: usize = 11;
    pub const SUB_NS_MAX: usize = 12;
    pub const SUB_LOCK_NS: usize = 13;
    pub const SUB_CHUNKS_SEEN: usize = 14;
    pub const SUB_NEW_CHUNK: usize = 15;
    pub const SUB_FAILED: usize = 16;
    pub const SUBFREE_CALLS: usize = 17;
    pub const SUBFREE_NS: usize = 18;
    pub const CLEARS: usize = 19;
    pub const CLEAR_BYTES: usize = 20;
    pub const CPU_CLEARS: usize = 21;
    pub const CPU_CLEAR_BYTES: usize = 22;
    pub const CLEAR_WAITS: usize = 23;
    pub const CLEAR_WAIT_NS: usize = 24;
    pub const CHUNKS_MADE: usize = 25;
    pub const CHUNKS_FREED: usize = 26;
    pub const KIND_BASE: usize = 32;
    pub const KIND_COUNT: usize = 7;
    pub const TYPE_BASE: usize = KIND_BASE + 2 * KIND_COUNT;
    pub const TYPE_COUNT: usize = 16;
    pub const TYPE_INFO_BASE: usize = TYPE_BASE + 2 * TYPE_COUNT;
    pub const HEAP_BASE: usize = TYPE_INFO_BASE + 2 * TYPE_COUNT;
    pub const HEAP_COUNT: usize = 4;
    pub const HEAPS: usize = HEAP_BASE + 3 * HEAP_COUNT;
    pub const TYPES: usize = HEAPS + 1;
    pub const WALK_BASE: usize = TYPES + 1;
    pub const WALK_FIELDS: usize = 7;
    pub const HOLE_BASE: usize = WALK_BASE + 2 * WALK_FIELDS;
    pub const HOLE_BUCKETS: usize = 5;
    pub const WALK_NS: usize = HOLE_BASE + 2 * HOLE_BUCKETS;
    pub const COUNT: usize = WALK_NS + 1;
}

pub const KINDS: [&str; at::KIND_COUNT] = [
    "buffers in chunks",
    "buffers of their own",
    "textures in chunks",
    "textures of their own",
    "heaps in chunks",
    "heaps of their own",
    "scratch",
];
pub const HOLES: [&str; at::HOLE_BUCKETS] = ["under 64 KiB", "64-256 KiB", "256 KiB-1 MiB", "1-4 MiB", "4 MiB and more"];

/// One row: its group, what it is, its figure and a second figure (a count,
/// One row: its group, what it is, its figure and a second figure (a count,
/// or 0 when it has none), and what the figure's bar is measured against.
pub type Row = (&'static str, String, f64, f64, f64);

/// `vkd3d_victory_memory_stats(device, out, count) -> counters known`.
pub type Export = unsafe extern "C" fn(*mut std::ffi::c_void, *mut u64, u32) -> u32;

/// The rows of two readings `seconds` apart (rates over that time).
pub fn lay_out(now: &[u64], before: &[u64], seconds: f64) -> Vec<Row> {
    const MB: f64 = (1u64 << 20) as f64;
    let rate = |i: usize| now[i].saturating_sub(before[i]) as f64 / seconds;
    // (milliseconds a second)
    let ms = |i: usize| rate(i) / 1e6;
    let mut rows: Vec<Row> = Vec::new();
    let card_budget = (0..(now[at::HEAPS] as usize).min(at::HEAP_COUNT))
        .map(|h| now[at::HEAP_BASE + 3 * h])
        .max()
        .unwrap_or(1)
        .max(1) as f64
        / MB;

    // The driver's heaps: use against budget.
    for h in 0..(now[at::HEAPS] as usize).min(at::HEAP_COUNT) {
        let [budget, used, size] = [0, 1, 2].map(|k| now[at::HEAP_BASE + 3 * h + k] as f64 / MB);
        rows.push(("driver heaps (MB used, MB budget)", format!("heap {h} of {size:.0} MB"), used, budget, budget.max(1.0)));
    }

    // Device memory by Vulkan memory type.
    for t in 0..(now[at::TYPES] as usize).min(at::TYPE_COUNT) {
        let (count, bytes) = (now[at::TYPE_BASE + 2 * t], now[at::TYPE_BASE + 2 * t + 1]);
        if count == 0 {
            continue;
        }
        let flags = now[at::TYPE_INFO_BASE + 2 * t];
        let heap = now[at::TYPE_INFO_BASE + 2 * t + 1];
        let mut what = Vec::new();
        if flags & 1 != 0 {
            what.push("card");
        }
        if flags & 2 != 0 {
            what.push("mapped");
        }
        if flags & 8 != 0 {
            what.push("cached");
        }
        if what.is_empty() {
            what.push("pc");
        }
        rows.push((
            "device memory by type (MB, allocations)",
            format!("type {t} heap {heap} {}", what.join("+")),
            bytes as f64 / MB,
            count as f64,
            card_budget,
        ));
    }

    for (k, name) in KINDS.iter().enumerate() {
        let (count, bytes) = (now[at::KIND_BASE + 2 * k], now[at::KIND_BASE + 2 * k + 1]);
        if count > 0 {
            rows.push(("live by kind (MB, count)", (*name).to_owned(), bytes as f64 / MB, count as f64, card_budget));
        }
    }

    for (i, name) in ["buffer chunks", "texture chunks"].into_iter().enumerate() {
        let w = |f: usize| now[at::WALK_BASE + at::WALK_FIELDS * i + f];
        if w(0) == 0 {
            continue;
        }
        let group = "chunks (MB, count)";
        rows.push((group, name.to_owned(), w(1) as f64 / MB, w(0) as f64, card_budget));
        rows.push((group, format!("{name}: free in them"), w(2) as f64 / MB, w(3) as f64, w(1).max(1) as f64 / MB));
        rows.push((group, format!("{name}: largest free run"), w(4) as f64 / MB, 0.0, 32.0));
        rows.push((group, format!("{name}: with none free"), w(5) as f64, 0.0, w(0) as f64));
        rows.push((group, format!("{name}: under a quarter used"), w(6) as f64, 0.0, w(0) as f64));
    }
    let free_all: u64 = (0..at::HOLE_BUCKETS).map(|b| now[at::HOLE_BASE + 2 * b + 1]).sum();
    for (b, name) in HOLES.iter().enumerate() {
        let (count, bytes) = (now[at::HOLE_BASE + 2 * b], now[at::HOLE_BASE + 2 * b + 1]);
        rows.push(("free runs by length (MB, count)", (*name).to_owned(), bytes as f64 / MB, count as f64, free_all.max(1) as f64 / MB));
    }

    // Calls, a second (ms a second against the whole second).
    let g = "calls (ms a second, calls a second)";
    let per = |calls: usize, ns: usize| (ms(ns), rate(calls));
    let mut call = |what: &str, (v, n): (f64, f64)| rows.push((g, what.to_owned(), v, n, 1000.0));
    call("vkAllocateMemory", per(at::ALLOC_CALLS, at::ALLOC_NS));
    call("vkFreeMemory", per(at::FREE_CALLS, at::FREE_NS));
    call("suballocate", per(at::SUB_CALLS, at::SUB_NS));
    call("suballocate: waiting for the lock", (ms(at::SUB_LOCK_NS), rate(at::SUB_CALLS)));
    call("suballocation given back", per(at::SUBFREE_CALLS, at::SUBFREE_NS));
    call("waits for a clear", per(at::CLEAR_WAITS, at::CLEAR_WAIT_NS));
    let g = "since the last reading (figure, count)";
    let longest = |i: usize| now[i] as f64 / 1e6;
    rows.push((g, "longest vkAllocateMemory (ms)".to_owned(), longest(at::ALLOC_NS_MAX), 0.0, 16.0));
    rows.push((g, "longest suballocation (ms)".to_owned(), longest(at::SUB_NS_MAX), 0.0, 16.0));
    let looked = rate(at::SUB_CHUNKS_SEEN) / rate(at::SUB_CALLS).max(1e-9);
    rows.push((g, "chunks looked at a suballocation".to_owned(), looked, 0.0, now[at::WALK_BASE].max(1) as f64 + now[at::WALK_BASE + at::WALK_FIELDS].max(1) as f64));
    rows.push((g, "clears queued (MB/s, a second)".to_owned(), rate(at::CLEAR_BYTES) / MB, rate(at::CLEARS), 1024.0));
    rows.push((g, "clears on the CPU (MB/s, a second)".to_owned(), rate(at::CPU_CLEAR_BYTES) / MB, rate(at::CPU_CLEARS), 1024.0));
    rows.push((g, "chunks made (a second)".to_owned(), rate(at::CHUNKS_MADE), rate(at::SUB_NEW_CHUNK), 10.0));
    rows.push((g, "chunks given back (a second)".to_owned(), rate(at::CHUNKS_FREED), 0.0, 10.0));
    rows.push((g, "allocations failed (a second)".to_owned(), rate(at::ALLOC_FAILS), rate(at::SUB_FAILED), 1.0));
    rows.push((g, "on a later memory type (a second)".to_owned(), rate(at::ALLOC_LATER_TYPE), 0.0, 1.0));
    rows.push((
        "since the start (MB, count)",
        "asked for the card, given the PC".to_owned(),
        now[at::ALLOC_OFF_CARD_BYTES] as f64 / MB,
        now[at::ALLOC_OFF_CARD] as f64,
        card_budget,
    ));
    rows.push(("since the start (MB, count)", "reading this (ms)".to_owned(), now[at::WALK_NS] as f64 / 1e6, 0.0, 16.0));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reading with two texture chunks of 16 MiB, 5 MiB free in 3 runs,
    /// and 100 suballocations over the second taking 50 ms.
    #[test]
    fn rows_name_the_chunks_holes_and_rates() {
        let mut now = vec![0u64; at::COUNT];
        let before = vec![0u64; at::COUNT];
        now[at::HEAPS] = 1;
        now[at::HEAP_BASE] = 20 << 30;
        now[at::HEAP_BASE + 1] = 18 << 30;
        now[at::HEAP_BASE + 2] = 24 << 30;
        let tex = at::WALK_BASE + at::WALK_FIELDS;
        now[tex] = 2;
        now[tex + 1] = 32 << 20;
        now[tex + 2] = 5 << 20;
        now[tex + 3] = 3;
        now[tex + 4] = 3 << 20;
        now[at::HOLE_BASE + 2 * 3] = 1;
        now[at::HOLE_BASE + 2 * 3 + 1] = 3 << 20;
        now[at::SUB_CALLS] = 100;
        now[at::SUB_NS] = 50_000_000;
        now[at::SUB_CHUNKS_SEEN] = 250;
        let rows = lay_out(&now, &before, 1.0);
        let find = |what: &str| rows.iter().find(|r| r.1 == what).unwrap_or_else(|| panic!("no row {what}: {rows:?}"));
        assert_eq!(find("heap 0 of 24576 MB").2, 18432.0);
        assert_eq!(find("texture chunks").3, 2.0);
        assert_eq!(find("texture chunks: free in them").2, 5.0);
        assert_eq!(find("texture chunks: largest free run").2, 3.0);
        assert_eq!(find("1-4 MiB").3, 1.0);
        let sub = find("suballocate");
        assert_eq!((sub.2, sub.3), (50.0, 100.0));
        assert_eq!(find("chunks looked at a suballocation").2, 2.5);
        assert!(rows.iter().all(|r| r.1 != "buffer chunks"), "no buffer chunks, no row");
    }
}
