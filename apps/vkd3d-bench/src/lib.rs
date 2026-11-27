//! Workloads for our vkd3d-proton's memory allocator, run through D3D12 on
//! the card with no title: each makes and lets go resources as a game
//! would, reads the allocator's counters (`vkd3d_stats`) as it goes,
//! checks they stay whole and reports a profile. The command line
//! (`main.rs`) runs them with any sizes; the tests run them small.

#![cfg(windows)]

use vkd3d_stats::at;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, PCWSTR};

type Create =
    unsafe extern "system" fn(*mut std::ffi::c_void, i32, *const windows::core::GUID, *mut *mut std::ffi::c_void) -> windows::core::HRESULT;

/// A device of our vkd3d-proton and its counters' export.
pub struct Own {
    device: ID3D12Device,
    stats: vkd3d_stats::Export,
}

/// Where our vkd3d-proton is when no folder is given: `VICTORY_VKD3D_DIR`,
/// else `~/.local/share/victory/vkd3d` (Wine passes the Unix home along as
/// HOME, or not; then the user's name).
pub fn default_dir() -> Option<String> {
    std::env::var("VICTORY_VKD3D_DIR")
        .ok()
        .or_else(|| std::env::var("HOME").ok().filter(|h| h.starts_with('/')).map(|h| format!("Z:{h}/.local/share/victory/vkd3d")))
        .or_else(|| std::env::var("USERNAME").ok().map(|u| format!("Z:/home/{u}/.local/share/victory/vkd3d")))
}

impl Own {
    /// Loads `d3d12core.dll` and `d3d12.dll` from `dir` (d3d12core first:
    /// d3d12.dll links to it by name and gets this one) and makes a device
    /// on the default adapter.
    pub fn new(dir: &str) -> Result<Own, String> {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let load = |name: &str| {
            let path: Vec<u16> = format!("{dir}/{name}").encode_utf16().chain([0]).collect();
            // SAFETY: a nul-terminated path.
            unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) }.map_err(|e| format!("{dir}/{name}: {e}"))
        };
        let core = load("d3d12core.dll")?;
        let d3d12 = load("d3d12.dll")?;
        // SAFETY: lookups of exports with these signatures.
        let (create, stats) = unsafe {
            (
                GetProcAddress(d3d12, windows::core::s!("D3D12CreateDevice")).ok_or("no D3D12CreateDevice")?,
                GetProcAddress(core, windows::core::s!("vkd3d_victory_memory_stats"))
                    .ok_or("this vkd3d-proton has no counters (native/vkd3d-victory-stats.patch)")?,
            )
        };
        // SAFETY: the exports' signatures.
        type Found = unsafe extern "system" fn() -> isize;
        let (create, stats) = unsafe {
            (std::mem::transmute::<Found, Create>(create), std::mem::transmute::<Found, vkd3d_stats::Export>(stats))
        };
        let mut raw = std::ptr::null_mut();
        // SAFETY: the default adapter, feature level 12_0, an out pointer of ours.
        let made = unsafe {
            create(std::ptr::null_mut(), windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0.0, &ID3D12Device::IID, &mut raw)
        };
        if made.is_err() || raw.is_null() {
            return Err(format!("no card device: {made:?}"));
        }
        // SAFETY: an owned reference the call returned.
        let device = unsafe { ID3D12Device::from_raw(raw) };
        Ok(Own { device, stats })
    }

    /// The counters now.
    pub fn read(&self) -> Vec<u64> {
        let mut out = vec![0u64; at::COUNT];
        // SAFETY: our vkd3d-proton's device; `out` holds `at::COUNT`.
        unsafe { (self.stats)(self.device.as_raw(), out.as_mut_ptr(), at::COUNT as u32) };
        out
    }

    /// A committed 2D texture in a default heap, as the host copies are
    /// made, and the bytes it takes.
    pub fn texture(&self, side: u64, mips: u16, format: DXGI_FORMAT) -> Option<(ID3D12Resource, u64)> {
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: side,
            Height: side as u32,
            DepthOrArraySize: 1,
            MipLevels: mips,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        self.committed(&desc, D3D12_HEAP_TYPE_DEFAULT)
    }

    /// A committed buffer in a heap of `kind` and the bytes it takes.
    pub fn buffer(&self, bytes: u64, kind: D3D12_HEAP_TYPE) -> Option<(ID3D12Resource, u64)> {
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

    fn committed(&self, desc: &D3D12_RESOURCE_DESC, kind: D3D12_HEAP_TYPE) -> Option<(ID3D12Resource, u64)> {
        let heap = D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() };
        let state = if kind == D3D12_HEAP_TYPE_UPLOAD { D3D12_RESOURCE_STATE_GENERIC_READ } else { D3D12_RESOURCE_STATE_COMMON };
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: plain descriptions and an out pointer of ours.
        unsafe { self.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, desc, state, None, &mut made) }.ok()?;
        let made = made?;
        // SAFETY: a plain query on a description.
        let bytes = unsafe { self.device.GetResourceAllocationInfo(0, &[*desc]) }.SizeInBytes;
        Some((made, bytes))
    }
}

/// A seeded xorshift: the same seed, the same workload.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    /// A number below `n` (0 for `n` of 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

const FORMATS: [DXGI_FORMAT; 5] = [
    DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_BC1_UNORM,
    DXGI_FORMAT_BC3_UNORM,
    DXGI_FORMAT_BC7_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT,
];

/// A texture of a world: 64 texels a side up to `64 << sides`, any of
/// [`FORMATS`], a full chain of levels or (one in four) one level.
fn world_texture(own: &Own, rng: &mut Rng, sides: u64) -> Option<(ID3D12Resource, u64)> {
    let side = 64u64 << rng.below(sides + 1);
    let format = FORMATS[rng.below(FORMATS.len() as u64) as usize];
    let mips = if rng.below(4) == 0 { 1 } else { side.trailing_zeros() as u16 + 1 };
    own.texture(side, mips, format)
}

/// [`world_texture`] placed in `pool` instead (the same draws of `rng`).
fn world_placed(own: &Own, pool: &std::sync::Arc<Pool>, rng: &mut Rng, sides: u64) -> Option<(ID3D12Resource, u64, Placed)> {
    let side = 64u64 << rng.below(sides + 1);
    let format = FORMATS[rng.below(FORMATS.len() as u64) as usize];
    let mips = if rng.below(4) == 0 { 1 } else { side.trailing_zeros() as u16 + 1 };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: side,
        Height: side as u32,
        DepthOrArraySize: 1,
        MipLevels: mips,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        ..Default::default()
    };
    Pool::place(pool, &own.device, &desc)
}

/// Textures placed in card heaps of our own (`CreatePlacedResource`), as
/// the layer could keep its copies instead of vkd3d-proton's chunks: heaps
/// of [`POOL_HEAP`] bytes, each with its free runs at 64 KiB, a texture in
/// the smallest run it fits of any heap (best fit), runs joined again when
/// a texture goes, a new heap when none fits. The streaming workload runs
/// with it (`--placed`) to set its waste and spread against vkd3d-proton's.
pub struct Pool {
    inner: std::sync::Mutex<PoolInner>,
}

/// A pool heap's bytes.
pub const POOL_HEAP: u64 = 64 << 20;

struct PoolInner {
    /// Each heap and its free runs (offset, length), in order.
    heaps: Vec<(ID3D12Heap, Vec<(u64, u64)>)>,
    /// Nanoseconds and calls of finding a run.
    ns: u64,
    calls: u64,
}

/// A texture's run in a pool heap, given back when this is dropped.
pub struct Placed {
    pool: std::sync::Arc<Pool>,
    heap: usize,
    offset: u64,
    len: u64,
}

impl Drop for Placed {
    fn drop(&mut self) {
        let mut inner = self.pool.inner.lock().unwrap_or_else(|e| e.into_inner());
        let runs = &mut inner.heaps[self.heap].1;
        let at = runs.partition_point(|r| r.0 < self.offset);
        runs.insert(at, (self.offset, self.len));
        // Joined with the runs either side when they touch.
        if at + 1 < runs.len() && runs[at].0 + runs[at].1 == runs[at + 1].0 {
            runs[at].1 += runs[at + 1].1;
            runs.remove(at + 1);
        }
        if at > 0 && runs[at - 1].0 + runs[at - 1].1 == runs[at].0 {
            runs[at - 1].1 += runs[at].1;
            runs.remove(at);
        }
    }
}

impl Pool {
    pub fn new() -> std::sync::Arc<Pool> {
        std::sync::Arc::new(Pool { inner: std::sync::Mutex::new(PoolInner { heaps: Vec::new(), ns: 0, calls: 0 }) })
    }

    /// `desc` placed in the smallest free run it fits, and the bytes it takes.
    fn place(pool: &std::sync::Arc<Pool>, device: &ID3D12Device, desc: &D3D12_RESOURCE_DESC) -> Option<(ID3D12Resource, u64, Placed)> {
        // SAFETY: a plain query on a description.
        let info = unsafe { device.GetResourceAllocationInfo(0, &[*desc]) };
        let len = info.SizeInBytes.next_multiple_of(64 << 10);
        if len > POOL_HEAP {
            return None;
        }
        let began = std::time::Instant::now();
        let mut inner = pool.inner.lock().unwrap_or_else(|e| e.into_inner());
        let best = inner
            .heaps
            .iter()
            .enumerate()
            .flat_map(|(h, (_, runs))| runs.iter().enumerate().map(move |(r, run)| (h, r, run.1)))
            .filter(|&(_, _, l)| l >= len)
            .min_by_key(|&(_, _, l)| l);
        let (heap, run) = match best {
            Some((h, r, _)) => (h, r),
            None => {
                let desc = D3D12_HEAP_DESC {
                    SizeInBytes: POOL_HEAP,
                    Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
                    Alignment: 64 << 10,
                    Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES | D3D12_HEAP_FLAG_CREATE_NOT_ZEROED,
                };
                let mut made: Option<ID3D12Heap> = None;
                // SAFETY: a plain description and an out pointer of ours.
                unsafe { device.CreateHeap(&desc, &mut made) }.ok()?;
                inner.heaps.push((made?, vec![(0, POOL_HEAP)]));
                (inner.heaps.len() - 1, 0)
            }
        };
        let offset = inner.heaps[heap].1[run].0;
        let runs = &mut inner.heaps[heap].1;
        if runs[run].1 == len {
            runs.remove(run);
        } else {
            runs[run].0 += len;
            runs[run].1 -= len;
        }
        inner.ns += began.elapsed().as_nanos() as u64;
        inner.calls += 1;
        let heap_object = inner.heaps[heap].0.clone();
        drop(inner);
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: a heap of ours, a free run that holds the resource, an out pointer of ours.
        let placed = unsafe { device.CreatePlacedResource(&heap_object, offset, desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made) };
        let slot = Placed { pool: pool.clone(), heap, offset, len };
        placed.ok()?;
        Some((made?, info.SizeInBytes, slot))
    }

    /// Heaps, their bytes, free bytes, free runs, the largest, and the mean
    /// microseconds of finding a run.
    fn state(&self) -> (u64, u64, u64, u64, u64, f64) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let runs = inner.heaps.iter().flat_map(|h| h.1.iter());
        let free: u64 = runs.clone().map(|r| r.1).sum();
        let largest = runs.clone().map(|r| r.1).max().unwrap_or(0);
        let n = inner.heaps.len() as u64;
        (n, n * POOL_HEAP, free, runs.count() as u64, largest, inner.ns as f64 / inner.calls.max(1) as f64 / 1e3)
    }

    fn line(&self) -> String {
        let (n, bytes, free, runs, largest, us) = self.state();
        format!(
            "our heaps {n} ({} MiB), {:.0}% free in {runs} runs, largest {} KiB; {us:.1} us a placement",
            bytes >> 20,
            free as f64 / bytes.max(1) as f64 * 100.0,
            largest >> 10
        )
    }
}

/// What a workload found: lines of its profile, and whether its checks held.
pub struct Report {
    pub lines: Vec<String>,
    pub failed: Vec<String>,
    /// The counters at the start and the end, and the seconds between.
    pub start: Vec<u64>,
    pub end: Vec<u64>,
    pub seconds: f64,
}

/// Index of a walk field of the texture chunks (1) or the buffer chunks (0).
fn walk(s: &[u64], chunks: usize, field: usize) -> u64 {
    s[at::WALK_BASE + at::WALK_FIELDS * chunks + field]
}

fn kind(s: &[u64], k: usize) -> (u64, u64) {
    (s[at::KIND_BASE + 2 * k], s[at::KIND_BASE + 2 * k + 1])
}

/// The texture chunks' state since `start`: chunks, their bytes, free in
/// them, free runs, the largest.
fn chunk_line(s: &[u64], start: &[u64]) -> String {
    let d = |f: usize| walk(s, 1, f).wrapping_sub(walk(start, 1, f));
    format!(
        "texture chunks {} ({} MiB), {:.0}% free in {} runs, largest {} KiB",
        d(0),
        d(1) >> 20,
        d(2) as f64 / d(1).max(1) as f64 * 100.0,
        d(3),
        walk(s, 1, 4) >> 10
    )
}

/// Suballocations between two readings: mean microseconds, chunks looked at.
fn sub_line(s: &[u64], before: &[u64]) -> String {
    let calls = s[at::SUB_CALLS].saturating_sub(before[at::SUB_CALLS]).max(1) as f64;
    format!(
        "{:.1} us and {:.1} chunks looked at a suballocation ({:.0} us waiting for the lock in all)",
        s[at::SUB_NS].saturating_sub(before[at::SUB_NS]) as f64 / calls / 1e3,
        s[at::SUB_CHUNKS_SEEN].saturating_sub(before[at::SUB_CHUNKS_SEEN]) as f64 / calls,
        s[at::SUB_LOCK_NS].saturating_sub(before[at::SUB_LOCK_NS]) as f64 / 1e3,
    )
}

/// The checks every workload makes at each step: each texture alive is
/// counted once (`textures` of them), and the texture chunks hold exactly
/// the bytes of the textures in them.
fn check_whole(now: &[u64], start: &[u64], textures: u64, at_what: &str, failed: &mut Vec<String>) {
    let counted = (kind(now, 2).0 - kind(start, 2).0) + (kind(now, 3).0 - kind(start, 3).0);
    if counted != textures {
        failed.push(format!("{at_what}: {counted} textures counted, {textures} alive"));
    }
    let used = |s: &[u64]| walk(s, 1, 1) - walk(s, 1, 2);
    let in_chunks = kind(now, 2).1 - kind(start, 2).1;
    if used(now) - used(start) != in_chunks {
        failed.push(format!("{at_what}: {} bytes used in texture chunks, {in_chunks} bytes of textures in them", used(now) - used(start)));
    }
}

fn finish(own: &Own, lines: Vec<String>, mut failed: Vec<String>, start: Vec<u64>, began: std::time::Instant) -> Report {
    let end = own.read();
    for k in 0..at::KIND_COUNT {
        if kind(&end, k) != kind(&start, k) {
            failed.push(format!("at the end: {} still counted {:?}", vkd3d_stats::KINDS[k], kind(&end, k)));
        }
    }
    if walk(&end, 1, 0) != walk(&start, 1, 0) {
        failed.push(format!("at the end: {} texture chunks not given back", walk(&end, 1, 0) - walk(&start, 1, 0)));
    }
    Report { lines, failed, start, end, seconds: began.elapsed().as_secs_f64() }
}

/// `count` textures of `side` texels (one level, RGBA8) made, every other
/// one let go, as many made again: the room let go stays in the chunks (no
/// chunk goes back, the device memory does not move) and the new ones take
/// it (no new chunk).
pub fn holes(own: &Own, count: usize, side: u64) -> Report {
    let began = std::time::Instant::now();
    let start = own.read();
    let (mut lines, mut failed) = (Vec::new(), Vec::new());
    let make = || own.texture(side, 1, DXGI_FORMAT_R8G8B8A8_UNORM);
    let mut all: Vec<Option<(ID3D12Resource, u64)>> = (0..count).map(|_| make()).collect();
    let bytes: u64 = all.iter().flatten().map(|t| t.1).sum();
    let full = own.read();
    let card = |s: &[u64]| (0..at::TYPE_COUNT).map(|t| s[at::TYPE_BASE + 2 * t + 1]).sum::<u64>();
    lines.push(format!("made {} of {} KiB: {}; {}", all.iter().flatten().count(), (bytes / count.max(1) as u64) >> 10, chunk_line(&full, &start), sub_line(&full, &start)));
    let mut gone = 0;
    for t in all.iter_mut().step_by(2) {
        gone += t.take().map_or(0, |t| t.1);
    }
    let half = own.read();
    lines.push(format!("every other let go ({} MiB): {}", gone >> 20, chunk_line(&half, &start)));
    if walk(&half, 1, 0) != walk(&full, 1, 0) {
        failed.push(format!("letting every other go gave {} chunks back", walk(&full, 1, 0) - walk(&half, 1, 0)));
    }
    if card(&half) != card(&full) {
        failed.push("letting every other go moved the device memory".to_owned());
    }
    let again: Vec<(ID3D12Resource, u64)> = (0..count.div_ceil(2)).filter_map(|_| make()).collect();
    let filled = own.read();
    lines.push(format!("{} made again: {}; {} chunks made; {}", again.len(), chunk_line(&filled, &start), filled[at::CHUNKS_MADE] - half[at::CHUNKS_MADE], sub_line(&filled, &half)));
    if filled[at::CHUNKS_MADE] != half[at::CHUNKS_MADE] {
        failed.push(format!("the new ones made {} chunks", filled[at::CHUNKS_MADE] - half[at::CHUNKS_MADE]));
    }
    check_whole(&filled, &start, (all.iter().flatten().count() + again.len()) as u64, "filled", &mut failed);
    drop(all);
    drop(again);
    finish(own, lines, failed, start, began)
}

/// `steps` steps of a seeded mix: textures (any size to 2048 a side, any
/// format and levels), card buffers and PC buffers made; the oldest or one
/// at random let go. A line and the checks every `every` steps.
pub fn random(own: &Own, rng: &mut Rng, steps: u32, every: u32) -> Report {
    let began = std::time::Instant::now();
    let start = own.read();
    let (mut lines, mut failed) = (Vec::new(), Vec::new());
    // (resource, bytes, 0 texture / 1 card buffer / 2 PC buffer)
    let mut live: std::collections::VecDeque<(ID3D12Resource, u64, u8)> = std::collections::VecDeque::new();
    let mut last = start.clone();
    for step in 0..steps {
        let roll = rng.below(100);
        if roll < 55 || live.len() < 64 {
            let made = match rng.below(10) {
                0..=6 => world_texture(own, rng, 5).map(|(t, b)| (t, b, 0)),
                7 | 8 => own.buffer((64 << 10) << rng.below(8), D3D12_HEAP_TYPE_DEFAULT).map(|(t, b)| (t, b, 1)),
                _ => own.buffer((64 << 10) << rng.below(5), D3D12_HEAP_TYPE_UPLOAD).map(|(t, b)| (t, b, 2)),
            };
            live.extend(made);
        } else if roll < 80 {
            live.pop_front();
        } else {
            let at = rng.below(live.len() as u64) as usize;
            live.remove(at);
        }
        if (step + 1) % every.max(1) == 0 {
            let now = own.read();
            let textures = live.iter().filter(|l| l.2 == 0).count() as u64;
            check_whole(&now, &start, textures, &format!("step {step}"), &mut failed);
            lines.push(format!("step {}: {} alive ({textures} textures); {}; {}", step + 1, live.len(), chunk_line(&now, &start), sub_line(&now, &last)));
            last = now;
        }
    }
    drop(live);
    finish(own, lines, failed, start, began)
}

/// How a streaming round picks what it lets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The textures of one to four whole chunks (a district unloaded).
    Few,
    /// One in two to five of the textures in a third of the chunks, at
    /// random (as the sweep lets go the least lately used).
    Scattered,
    /// Either, at random each round.
    Mixed,
}

/// A streaming world: `textures` textures (64 to 1024 texels a side, any
/// format and levels, under 2 MiB) loaded at once, then `rounds` rounds of
/// newly loaded ones: each lets go by `pick` and loads as many bytes of
/// textures of other sizes (to 512 a side). vkd3d-proton places a load in
/// its chunks in order, so the textures made in a row up to 16 MiB stand for
/// a chunk (their group). Checks the counts stay whole each round and that
/// the chunks never hold more than the world's bytes and a quarter, and
/// two chunks (the new load goes into the room let go).
///
/// With `pool` the textures are placed in heaps of our own ([`Pool`])
/// instead: the same draws, its line in place of the chunks', and its bytes
/// held to the same bound.
pub fn streaming(own: &Own, rng: &mut Rng, textures: usize, rounds: u32, pick: Pick, pool: Option<std::sync::Arc<Pool>>) -> Report {
    const CHUNK: u64 = 16 << 20;
    let began = std::time::Instant::now();
    let start = own.read();
    let (mut lines, mut failed) = (Vec::new(), Vec::new());
    let make = |rng: &mut Rng, sides: u64| match &pool {
        Some(pool) => world_placed(own, pool, rng, sides).map(|(t, b, p)| (t, b, Some(p))),
        None => world_texture(own, rng, sides).map(|(t, b)| (t, b, None)),
    };
    // Each kind's line, and the bytes its textures take on the card.
    let state = |now: &[u64]| match &pool {
        Some(pool) => (pool.line(), pool.state().1),
        None => (chunk_line(now, &start), walk(now, 1, 1) - walk(&start, 1, 1)),
    };
    // (texture, bytes, its group, its run in a pool heap)
    let mut world: Vec<(ID3D12Resource, u64, u32, Option<Placed>)> = Vec::with_capacity(textures);
    let (mut group, mut in_group) = (0u32, 0u64);
    while world.len() < textures {
        let Some((t, bytes, placed)) = make(rng, 4) else { continue };
        if bytes >= 2 << 20 {
            continue;
        }
        if in_group + bytes > CHUNK {
            (group, in_group) = (group + 1, 0);
        }
        in_group += bytes;
        world.push((t, bytes, group, placed));
    }
    let live_bytes = |w: &[(ID3D12Resource, u64, u32, Option<Placed>)]| w.iter().map(|t| t.1).sum::<u64>();
    let loaded = own.read();
    lines.push(format!("load: {} textures, {} MiB in {} groups; {}; {}", world.len(), live_bytes(&world) >> 20, group + 1, state(&loaded).0, sub_line(&loaded, &start)));
    if pool.is_none() {
        check_whole(&loaded, &start, world.len() as u64, "load", &mut failed);
    }
    let mut before = loaded;
    for round in 0..rounds {
        let few = match pick {
            Pick::Few => true,
            Pick::Scattered => false,
            Pick::Mixed => rng.below(2) == 0,
        };
        let mut groups: Vec<u32> = world.iter().map(|t| t.2).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        groups.sort_unstable();
        let picked: std::collections::HashSet<u32> = if few {
            (0..1 + rng.below(4)).map(|_| groups[rng.below(groups.len() as u64) as usize]).collect()
        } else {
            groups.iter().copied().filter(|_| rng.below(3) == 0).collect()
        };
        let share = if few { 1 } else { 2 + rng.below(4) };
        let mut gone = 0u64;
        let mut kept = Vec::with_capacity(world.len());
        for t in world.drain(..) {
            if picked.contains(&t.2) && rng.below(share) == 0 {
                gone += t.1;
            } else {
                kept.push(t);
            }
        }
        world = kept;
        let let_go = own.read();
        let mut made = 0u64;
        while made < gone {
            let Some((t, bytes, placed)) = make(rng, 3) else { continue };
            made += bytes;
            world.push((t, bytes, 1_000_000 + round, placed));
        }
        let now = own.read();
        if pool.is_none() {
            check_whole(&now, &start, world.len() as u64, &format!("round {round}"), &mut failed);
        }
        lines.push(format!(
            "round {round} ({}, {} groups, 1 in {share}): {} MiB let go, {} chunks back; {} MiB loaded, {} chunks made; {} for {} MiB; {}",
            if few { "few" } else { "scattered" },
            picked.len(),
            gone >> 20,
            let_go[at::CHUNKS_FREED] - before[at::CHUNKS_FREED],
            made >> 20,
            now[at::CHUNKS_MADE] - let_go[at::CHUNKS_MADE],
            state(&now).0,
            live_bytes(&world) >> 20,
            sub_line(&now, &let_go),
        ));
        let chunk_bytes = state(&now).1;
        let world_bytes = live_bytes(&world);
        if chunk_bytes > world_bytes + world_bytes / 4 + 2 * CHUNK {
            failed.push(format!("round {round}: {} MiB of chunks for {} MiB of textures", chunk_bytes >> 20, world_bytes >> 20));
        }
        before = now;
    }
    drop(world);
    // (The pool's heaps go with it, before the end's checks.)
    drop(pool);
    finish(own, lines, failed, start, began)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The allocator's counters are the process's, not a device's: one
    /// workload at a time, or one counts another's resources.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Our vkd3d-proton and a card, or why not (the test then passes).
    fn own() -> Option<Own> {
        let dir = default_dir()?;
        match Own::new(&dir) {
            Ok(own) => Some(own),
            Err(e) => {
                println!("{e}; skipped");
                None
            }
        }
    }

    fn held(report: &Report) {
        for line in &report.lines {
            println!("{line}");
        }
        assert!(report.failed.is_empty(), "{:#?}", report.failed);
    }

    #[test]
    fn a_texture_let_go_leaves_a_hole_that_a_new_one_fills() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(own) = own() else { return };
        held(&holes(&own, 96, 512));
    }

    #[test]
    fn a_random_workload_keeps_the_counts_whole() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(own) = own() else { return };
        held(&random(&own, &mut Rng::new(0x9e37_79b9_7f4a_7c15), 3000, 500));
    }

    #[test]
    fn a_streaming_world_reuses_the_room_it_lets_go() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(own) = own() else { return };
        held(&streaming(&own, &mut Rng::new(0x2545_f491_4f6c_dd1d), 3000, 12, Pick::Mixed, None));
    }

    /// The same world placed in heaps of our own: the counts stay whole
    /// (nothing of it is in vkd3d-proton's chunks) and the heaps hold to
    /// the same bound.
    #[test]
    fn a_streaming_world_in_our_heaps_reuses_the_room_it_lets_go() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let Some(own) = own() else { return };
        held(&streaming(&own, &mut Rng::new(0x2545_f491_4f6c_dd1d), 3000, 12, Pick::Mixed, Some(Pool::new())));
    }
}
