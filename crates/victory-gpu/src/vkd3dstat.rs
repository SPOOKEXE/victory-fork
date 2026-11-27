//! vkd3d-proton's memory allocator, from inside: the counters
//! `native/vkd3d-victory-stats.patch` adds (its `libs/vkd3d/victory_stats.h`
//! names the same indices), read through the `d3d12core.dll` export
//! `vkd3d_victory_memory_stats`. Shown by the overlay's vkd3d view (F2) and
//! written into each snapshot (F1). Without the patched vkd3d-proton
//! (`native/build-vkd3d.sh`) there is no export and nothing to show.

use std::sync::Mutex;

/// The export's indices (`enum vkd3d_victory_stat`).
mod at {
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

const KINDS: [&str; at::KIND_COUNT] = [
    "buffers in chunks",
    "buffers of their own",
    "textures in chunks",
    "textures of their own",
    "heaps in chunks",
    "heaps of their own",
    "scratch",
];
const HOLES: [&str; at::HOLE_BUCKETS] = ["under 64 KiB", "64-256 KiB", "256 KiB-1 MiB", "1-4 MiB", "4 MiB and more"];

/// One row: its group, what it is, its figure and a second figure (a count,
/// or 0 when it has none), and what the figure's bar is measured against.
pub(crate) type Row = (&'static str, String, f64, f64, f64);

type Export = unsafe extern "C" fn(*mut std::ffi::c_void, *mut u64, u32) -> u32;

fn export() -> Option<Export> {
    static EXPORT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let at = (*EXPORT.get_or_init(|| {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
        // SAFETY: plain lookups of a loaded module and one of its exports.
        unsafe {
            let module = GetModuleHandleA(c"d3d12core.dll".as_ptr().cast());
            if module.is_null() {
                return None;
            }
            GetProcAddress(module, c"vkd3d_victory_memory_stats".as_ptr().cast()).map(|f| f as usize)
        }
    }))?;
    // SAFETY: the export has this signature (`include/vkd3d.h` of the patch).
    Some(unsafe { std::mem::transmute::<usize, Export>(at) })
}

/// The counters now, if the export is there and the host device is made.
fn read() -> Option<Vec<u64>> {
    use windows::core::Interface;
    let export = export()?;
    let host = crate::host::host()?;
    let mut out = vec![0u64; at::COUNT];
    // SAFETY: the host's device is vkd3d-proton's own (no layer between),
    // and `out` holds `at::COUNT` values.
    let known = unsafe { export(host.device.as_raw(), out.as_mut_ptr(), at::COUNT as u32) };
    (known as usize >= at::COUNT).then_some(out)
}

/// The last reading and when (ns), for the rates.
static LAST: Mutex<Option<(u64, Vec<u64>)>> = Mutex::new(None);

/// The allocator as rows, rates over the time since the last call (the
/// first call's rates are since the start). Empty without the export.
pub(crate) fn rows() -> Vec<Row> {
    let Some(now) = read() else {
        return Vec::new();
    };
    let at_ns = crate::host::now_ms() * 1_000_000;
    let (seconds, before) = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let before = last.replace((at_ns, now.clone()));
        match before {
            Some((then, values)) if at_ns > then => ((at_ns - then) as f64 / 1e9, values),
            _ => (1.0, vec![0; at::COUNT]),
        }
    };
    lay_out(&now, &before, seconds)
}

/// [`rows`] from two readings `seconds` apart.
fn lay_out(now: &[u64], before: &[u64], seconds: f64) -> Vec<Row> {
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

/// vkd3d-proton's allocator driven on the card with workloads of our own,
/// read back through the same counters: needs the vkd3d-proton of
/// `native/build-vkd3d.sh` (in `VICTORY_VKD3D_DIR`, else the default
/// `~/.local/share/victory/vkd3d`) and a card; without them each test says
/// so and passes. `cargo test -p victory-gpu vkd3d_alloc -- --nocapture`
/// prints each workload's profile (chunks, holes, the time a suballocation
/// takes).
#[cfg(test)]
mod vkd3d_alloc_tests {
    use super::at;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::core::{Interface, PCWSTR};

    type Create = unsafe extern "system" fn(*mut std::ffi::c_void, i32, *const windows::core::GUID, *mut *mut std::ffi::c_void) -> windows::core::HRESULT;

    /// A device of our vkd3d-proton and its counters' export.
    struct Own {
        device: ID3D12Device,
        stats: super::Export,
    }

    impl Own {
        fn new() -> Option<Own> {
            use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
            // (Wine passes the Unix home along as HOME, or not; then the user's name.)
            let dir = std::env::var("VICTORY_VKD3D_DIR")
                .ok()
                .or_else(|| std::env::var("HOME").ok().filter(|h| h.starts_with('/')).map(|h| format!("Z:{h}/.local/share/victory/vkd3d")))
                .or_else(|| std::env::var("USERNAME").ok().map(|u| format!("Z:/home/{u}/.local/share/victory/vkd3d")));
            let Some(dir) = dir else {
                println!("no VICTORY_VKD3D_DIR, HOME or USERNAME; skipped");
                return None;
            };
            let load = |name: &str| {
                let path: Vec<u16> = format!("{dir}/{name}").encode_utf16().chain([0]).collect();
                // SAFETY: a nul-terminated path.
                unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) }.ok()
            };
            // d3d12core first: d3d12.dll links to it by name, and gets this one.
            let Some(core) = load("d3d12core.dll") else {
                println!("no vkd3d-proton of our own in {dir}; skipped");
                return None;
            };
            let Some(d3d12) = load("d3d12.dll") else {
                println!("no d3d12.dll beside d3d12core.dll in {dir}; skipped");
                return None;
            };
            // SAFETY: lookups of exports with these signatures.
            let (create, stats) = unsafe {
                (GetProcAddress(d3d12, windows::core::s!("D3D12CreateDevice"))?, GetProcAddress(core, windows::core::s!("vkd3d_victory_memory_stats")))
            };
            let Some(stats) = stats else {
                println!("vkd3d-proton in {dir} has no counters (native/vkd3d-victory-stats.patch); skipped");
                return None;
            };
            // SAFETY: the exports' signatures.
            let (create, stats) = unsafe { (std::mem::transmute::<_, Create>(create), std::mem::transmute::<_, super::Export>(stats)) };
            let mut raw = std::ptr::null_mut();
            // SAFETY: the default adapter, feature level 12_0, an out pointer of ours.
            let made = unsafe { create(std::ptr::null_mut(), windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0.0, &ID3D12Device::IID, &mut raw) };
            if made.is_err() || raw.is_null() {
                println!("no card device ({made:?}); skipped");
                return None;
            }
            // SAFETY: an owned reference the call returned.
            let device = unsafe { ID3D12Device::from_raw(raw) };
            Some(Own { device, stats })
        }

        fn read(&self) -> Vec<u64> {
            let mut out = vec![0u64; at::COUNT];
            // SAFETY: our vkd3d-proton's device; `out` holds `at::COUNT`.
            unsafe { (self.stats)(self.device.as_raw(), out.as_mut_ptr(), at::COUNT as u32) };
            out
        }

        /// A committed texture in a default heap, as the host copies are made.
        fn texture(&self, width: u64, height: u32, mips: u16, format: DXGI_FORMAT) -> Option<ID3D12Resource> {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: width,
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: mips,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            self.committed(&desc, D3D12_HEAP_TYPE_DEFAULT)
        }

        fn buffer(&self, bytes: u64, kind: D3D12_HEAP_TYPE) -> Option<ID3D12Resource> {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: bytes,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            self.committed(&desc, kind)
        }

        fn committed(&self, desc: &D3D12_RESOURCE_DESC, kind: D3D12_HEAP_TYPE) -> Option<ID3D12Resource> {
            let heap = D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() };
            let state = if kind == D3D12_HEAP_TYPE_UPLOAD { D3D12_RESOURCE_STATE_GENERIC_READ } else { D3D12_RESOURCE_STATE_COMMON };
            let mut made: Option<ID3D12Resource> = None;
            // SAFETY: plain descriptions and an out pointer of ours.
            unsafe { self.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, desc, state, None, &mut made) }.ok()?;
            made
        }
    }

    /// Bytes used in the texture chunks (index 1) or the buffer chunks (0).
    fn used(s: &[u64], i: usize) -> u64 {
        s[at::WALK_BASE + at::WALK_FIELDS * i + 1] - s[at::WALK_BASE + at::WALK_FIELDS * i + 2]
    }

    fn kind(s: &[u64], k: usize) -> (u64, u64) {
        (s[at::KIND_BASE + 2 * k], s[at::KIND_BASE + 2 * k + 1])
    }

    /// The vkd3d texture chunks keep the room of textures let go: no chunk
    /// goes back while one of its textures lives, its room is free in it,
    /// and new textures of the size take that room before a new chunk is
    /// made (what `crate::resource::holes` counts on).
    #[test]
    fn a_texture_let_go_leaves_a_hole_that_a_new_one_fills() {
        let Some(own) = Own::new() else { return };
        let start = own.read();
        // 1 MiB each (512 x 512 x 4): in 16 MiB chunks.
        let mut all: Vec<Option<ID3D12Resource>> = (0..96).map(|_| own.texture(512, 512, 1, DXGI_FORMAT_R8G8B8A8_UNORM)).collect();
        assert!(all.iter().all(Option::is_some), "96 textures made");
        let full = own.read();
        let chunks = full[at::WALK_BASE + at::WALK_FIELDS];
        assert!(chunks >= 6, "96 MiB of textures in at least six chunks: {chunks}");
        assert_eq!(kind(&full, 2).0 - kind(&start, 2).0, 96, "96 textures in chunks");
        let card_bytes = |s: &[u64]| (0..at::TYPE_COUNT).map(|t| s[at::TYPE_BASE + 2 * t + 1]).sum::<u64>();

        // Every other one let go: half the room, no chunk back.
        for t in all.iter_mut().step_by(2) {
            *t = None;
        }
        let half = own.read();
        assert_eq!(half[at::WALK_BASE + at::WALK_FIELDS], chunks, "no chunk empties");
        assert_eq!(used(&full, 1) - used(&half, 1), 48 << 20, "48 MiB free in the chunks");
        assert_eq!(card_bytes(&half), card_bytes(&full), "the device memory in use does not move");

        // 48 new ones of the size: in the holes, no new chunk.
        let again: Vec<ID3D12Resource> = (0..48).filter_map(|_| own.texture(512, 512, 1, DXGI_FORMAT_R8G8B8A8_UNORM)).collect();
        assert_eq!(again.len(), 48);
        let filled = own.read();
        assert_eq!(filled[at::WALK_BASE + at::WALK_FIELDS], chunks, "the holes are taken first");
        assert_eq!(used(&filled, 1), used(&full, 1));
        assert_eq!(filled[at::CHUNKS_MADE] - half[at::CHUNKS_MADE], 0);

        // Everything let go: the chunks go back.
        drop(all);
        drop(again);
        let empty = own.read();
        assert_eq!(empty[at::WALK_BASE + at::WALK_FIELDS], start[at::WALK_BASE + at::WALK_FIELDS], "every chunk back");
        println!(
            "{chunks} chunks for 96 MiB; {} chunks made, {} given back; suballocation {:.1} us on average",
            empty[at::CHUNKS_MADE] - start[at::CHUNKS_MADE],
            empty[at::CHUNKS_FREED] - start[at::CHUNKS_FREED],
            (empty[at::SUB_NS] - start[at::SUB_NS]) as f64 / (empty[at::SUB_CALLS] - start[at::SUB_CALLS]).max(1) as f64 / 1e3
        );
    }

    /// A seeded mix of textures (sizes, formats, levels) and buffers (card
    /// and PC), let go at random or oldest first, as a streaming world
    /// does. At every checkpoint the counts are whole: the bytes used in
    /// the chunks are the bytes of the resources in them, and each kind's
    /// count is the resources of that kind alive. Prints the profile.
    #[test]
    fn a_random_workload_keeps_the_counts_whole() {
        let Some(own) = Own::new() else { return };
        let start = own.read();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        // (resource, bytes the host says it takes, 0 texture / 1 card buffer / 2 PC buffer)
        let mut live: std::collections::VecDeque<(ID3D12Resource, u64, u8)> = std::collections::VecDeque::new();
        let formats = [DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_BC1_UNORM, DXGI_FORMAT_BC7_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT];
        let mut profile = Vec::new();
        for step in 0..3000u32 {
            let roll = next(100);
            if roll < 55 || live.len() < 64 {
                let made = match next(10) {
                    0..=6 => {
                        let side = 64u64 << next(6);
                        let format = formats[next(formats.len() as u64) as usize];
                        let mips = 1 + next(side.trailing_zeros() as u64 + 1) as u16;
                        own.texture(side, side as u32, mips, format).map(|t| (t, 0))
                    }
                    7 | 8 => own.buffer((64 << 10) << next(8), D3D12_HEAP_TYPE_DEFAULT).map(|b| (b, 1)),
                    _ => own.buffer((64 << 10) << next(5), D3D12_HEAP_TYPE_UPLOAD).map(|b| (b, 2)),
                };
                if let Some((r, k)) = made {
                    // SAFETY: a plain query on a live resource.
                    let bytes = unsafe { own.device.GetResourceAllocationInfo(0, &[r.GetDesc()]) }.SizeInBytes;
                    live.push_back((r, bytes, k));
                }
            } else if roll < 80 {
                // the oldest (a sweep's least lately used)
                live.pop_front();
            } else {
                let at = next(live.len() as u64) as usize;
                live.remove(at);
            }
            if step % 500 == 499 {
                let now = own.read();
                let textures = live.iter().filter(|l| l.2 == 0).count() as u64;
                let (in_chunks, own_ones) = (kind(&now, 2).0 - kind(&start, 2).0, kind(&now, 3).0 - kind(&start, 3).0);
                assert_eq!(in_chunks + own_ones, textures, "every texture alive is counted once, step {step}");
                assert_eq!(used(&now, 1) - used(&start, 1), kind(&now, 2).1 - kind(&start, 2).1, "texture chunks hold exactly the textures in them, step {step}");
                let chunk = |f: usize| now[at::WALK_BASE + at::WALK_FIELDS + f] - start[at::WALK_BASE + at::WALK_FIELDS + f];
                let calls = (now[at::SUB_CALLS] - start[at::SUB_CALLS]).max(1) as f64;
                profile.push(format!(
                    "step {step}: {} alive ({textures} textures, {in_chunks} in chunks); texture chunks {} ({} MiB), {:.0}% free in {} runs, largest {} KiB; {:.1} chunks looked at and {:.1} us a suballocation",
                    live.len(),
                    chunk(0),
                    chunk(1) >> 20,
                    chunk(2) as f64 / chunk(1).max(1) as f64 * 100.0,
                    chunk(3),
                    now[at::WALK_BASE + at::WALK_FIELDS + 4] >> 10,
                    (now[at::SUB_CHUNKS_SEEN] - start[at::SUB_CHUNKS_SEEN]) as f64 / calls,
                    (now[at::SUB_NS] - start[at::SUB_NS]) as f64 / calls / 1e3,
                ));
            }
        }
        drop(live);
        let end = own.read();
        for k in 0..at::KIND_COUNT {
            assert_eq!(kind(&end, k), kind(&start, k), "kind {k}: nothing left counted");
        }
        for line in profile {
            println!("{line}");
        }
        println!(
            "clears queued {} ({} MiB), on the CPU {}; chunks made {} given back {}",
            end[at::CLEARS] - start[at::CLEARS],
            (end[at::CLEAR_BYTES] - start[at::CLEAR_BYTES]) >> 20,
            end[at::CPU_CLEARS] - start[at::CPU_CLEARS],
            end[at::CHUNKS_MADE] - start[at::CHUNKS_MADE],
            end[at::CHUNKS_FREED] - start[at::CHUNKS_FREED]
        );
    }
}
