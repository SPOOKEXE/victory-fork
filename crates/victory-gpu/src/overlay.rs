//! The in-game overlay (F9; F10 the card, F8 memory, F7 a time line): what a frame's time goes to, span by span,
//! drawn by us on the processor and copied onto the back buffer just
//! before the present. No UI library: labelled bars in a 5x7 font with a
//! dark outline and no panel behind them (only the lit pixels are copied,
//! a box a run of them), so the game shows through. While it is up the
//! title does not see the arrow keys: Up and Down scroll the rows, Left
//! and Right show fewer or more spans per thread.
//!
//! The rows are the frame graph's spans (`crate::trace`, timed whenever
//! the overlay is up), summed by name and thread over the last half
//! second and given per frame, thread by thread, heaviest first; the
//! card's rows are its own time per list (`crate::cardmeter`). Above them
//! the frame's length and a few totals of the frame log
//! (`crate::framelog`). `VICTORY_OVERLAY=1` shows it from the start.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use crate::framelog::T;

pub(crate) const WIDTH: u32 = 500;
pub(crate) const HEIGHT: u32 = 300;
/// Frames kept for the percentiles.
const KEPT: usize = 240;
/// Rows shown at once (Up and Down move through the rest).
const ROWS: usize = 28;
const ROW: u32 = 9;
/// Spans shown per thread at each depth (Left and Right).
const DEPTHS: [usize; 4] = [1, 3, 6, usize::MAX];

static SHOWN: AtomicBool = AtomicBool::new(false);
/// Which view is up: 0 the processor's spans (F9), 1 the card's time by
/// pipeline (F10), 2 where the memory is (F8), 3 one frame's spans in order
/// on a time line, the processor's threads and the card's lists (F7).
static VIEW: AtomicU32 = AtomicU32::new(0);
/// The first row shown, and the depth.
static SCROLL: AtomicU32 = AtomicU32::new(0);
static DEPTH: AtomicU32 = AtomicU32::new(3);

struct State {
    /// Frame lengths (ms), oldest first.
    frames: std::collections::VecDeque<f32>,
    last_ns: u64,
    /// The frame log's totals at the last present, their sums since the
    /// last half second, and per-frame means of the half second before.
    totals: Vec<u64>,
    sums: Vec<u64>,
    mean: Vec<f64>,
    /// Spans by (thread group, name): nanoseconds and count since the last
    /// half second; then per frame over the half second before (ms, count).
    spans: HashMap<(String, &'static str), (u64, u32)>,
    rows: Vec<(String, &'static str, f64, f64)>,
    /// The card's time by pipeline (F10): microseconds and count since the
    /// last half second, then per frame (ms, count) over the one before.
    gpu: HashMap<String, (u64, u32)>,
    gpu_rows: Vec<(String, f64, f64)>,
    /// Where the memory is (F8; `crate::copystat::memory_rows`), taken
    /// every two seconds while that view is up, and when.
    memory_rows: Vec<(&'static str, String, u64, u64)>,
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    memory_at: u64,
    summed: u32,
    since_ns: u64,
    vram: (u64, u64),
    /// F9, Up, Down, Left, Right, F10, F8, F7, F1 as last seen.
    keys: [bool; 9],
    /// The time line (F7), kept only while it is up: spans of the last
    /// [`KEEP_NS`] (start ns, length ns, thread group, name), the presents'
    /// times, and the frame laid out at the last half second.
    timeline: std::collections::VecDeque<(u64, u64, u8, &'static str)>,
    presents: std::collections::VecDeque<u64>,
    laid: Option<Laid>,
    /// A snapshot being recorded (F1, `VICTORY_SNAPSHOT_AT`).
    snap: Option<Snap>,
}

/// How long a snapshot records, and how long after that it waits for the
/// card's times (they arrive about a second and a half late).
const SNAP_NS: u64 = 2_500_000_000;
const SNAP_WAIT_NS: u64 = 2_000_000_000;

/// A snapshot (F1): everything the overlay's views show, over [`SNAP_NS`],
/// written as Markdown to `snapshots/<time>.md` under the run's folder.
struct Snap {
    started: u64,
    totals: Vec<u64>,
    frames: Vec<f32>,
    spans: HashMap<(String, &'static str), (u64, u32)>,
    gpu: HashMap<String, (u64, u32)>,
}

/// Whether a snapshot is being recorded: spans and the card's pipelines are
/// timed then, whatever view is up (`crate::trace::on`, `crate::cardmeter`).
pub(crate) fn recording() -> bool {
    RECORDING.load(Relaxed)
}
static RECORDING: AtomicBool = AtomicBool::new(false);

/// How long the time line keeps spans: the card's times arrive about a
/// second and a half after the frame they belong to.
const KEEP_NS: u64 = 4_000_000_000;
/// The frame shown on the time line is the last one that ended this long ago.
const SHOW_BEHIND_NS: u64 = 2_000_000_000;

/// A frame laid out for the time line: its length, and its spans (start and
/// length in ms from the frame's start, thread group, depth inside that
/// group's other spans, name).
struct Laid {
    ms: f64,
    spans: Vec<(f64, f64, u8, u8, &'static str)>,
}

/// Thread groups of the time line, in the order of its rows.
const GROUPS: [&str; 6] = ["title render thread", "title-main", "victory-submit", "victory-wide", "victory-relay", "card"];

fn group_index(group: &str) -> u8 {
    GROUPS.iter().position(|g| *g == group).unwrap_or(GROUPS.len()) as u8
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Thread groups by thread number: names with their numbers taken off
/// (the six wide workers are one group).
static THREADS: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

/// Whether the overlay is up: F9 toggles it (with the title's window in
/// front), `VICTORY_OVERLAY=1` starts with it.
pub fn shown() -> bool {
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        if std::env::var_os("VICTORY_OVERLAY").is_some_and(|v| v == "1") {
            SHOWN.store(true, Relaxed);
        }
    });
    SHOWN.load(Relaxed)
}

/// A card-view row's text for the pipeline the card meter names `name` (its shaders as
/// `[stage hash, ...]`): the shaders' own names where their containers have them
/// (`crate::pipeline::label`, the compute or mesh one first), then the first hash's first
/// six digits to tell variants apart; at most `NAME_CHARS` characters.
fn pipeline_text(name: &str) -> String {
    const NAME_CHARS: usize = 37;
    let inner = name.trim_matches(|c| c == '[' || c == ']');
    let mut names = Vec::new();
    let mut first_hash = None;
    for part in inner.split(", ") {
        let Some((stage, hash)) = part.split_once(' ') else { continue };
        first_hash.get_or_insert(hash);
        match crate::pipeline::label(hash) {
            // (the stage is in the name already: CS_, PS_, MS_ ...)
            Some(label) => names.push(label),
            None => names.push(format!("{stage} {hash}")),
        }
    }
    if names.is_empty() {
        return inner.to_uppercase();
    }
    let mut text = names.join("+");
    if let Some(hash) = first_hash {
        let short: String = hash.chars().take(6).collect();
        let room = NAME_CHARS.saturating_sub(short.len() + 1);
        if text.chars().count() > room {
            text = text.chars().take(room).collect();
        }
        text = format!("{text} {short}");
    }
    text.to_uppercase()
}

/// Whether the time line (F7) is up: the card meter then times every
/// pipeline, to show them inside the card's lists.
pub(crate) fn timeline_shown() -> bool {
    shown() && VIEW.load(Relaxed) == 3
}

/// `name` as a string that lives as long as the process: the card's pipelines
/// on the time line, named by the meter's text (about as many as there are
/// pipelines, each kept once).
pub(crate) fn interned(name: &str) -> &'static str {
    static NAMES: Mutex<Option<std::collections::HashSet<&'static str>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let names = names.get_or_insert_with(Default::default);
    if let Some(&n) = names.get(name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(n);
    n
}

/// Whether the card's view (F10) is up: the card meter then times every
/// pipeline (`crate::cardmeter::pipelines`).
pub(crate) fn gpu_shown() -> bool {
    shown() && VIEW.load(Relaxed) == 1
}

/// The card ran pipeline `name` for `us` (a pair of the card meter, read
/// some frames after it ran).
pub(crate) fn gpu_span(name: &str, us: u64) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = state.as_mut() {
        let e = s.gpu.entry(name.to_owned()).or_insert((0, 0));
        e.0 += us;
        e.1 += 1;
        // (the card's times of the frames recorded arrive within the wait after them)
        if let Some(snap) = s.snap.as_mut() {
            let e = snap.gpu.entry(name.to_owned()).or_insert((0, 0));
            e.0 += us;
            e.1 += 1;
        }
    }
}

/// A thread's name, once, as it first times a span.
pub(crate) fn thread_name(tid: u32, name: &str) {
    let group = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ');
    let group = if group == "thread" { "title render thread" } else { group };
    THREADS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(tid, group.to_owned());
}

/// A span ended: `ns` long, on thread `tid` (`crate::trace::CARD_TRACK`
/// for the card's). Gathered per thread and handed over 128 at a time:
/// thousands a frame from seven threads into one lock was a wait of its own.
pub(crate) fn span(name: &'static str, tid: u32, start: u64, ns: u64) {
    thread_local! {
        static HERE: std::cell::RefCell<Vec<(&'static str, u32, u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    HERE.with(|here| {
        let mut here = here.borrow_mut();
        here.push((name, tid, start, ns));
        if here.len() >= 128 || tid == crate::trace::CARD_TRACK {
            hand_over(&mut here);
        }
    });
}

fn hand_over(spans: &mut Vec<(&'static str, u32, u64, u64)>) {
    let threads = THREADS.lock().unwrap_or_else(|e| e.into_inner());
    let group = |tid: u32| {
        if tid == crate::trace::CARD_TRACK {
            "card".to_owned()
        } else {
            threads.as_ref().and_then(|t| t.get(&tid).cloned()).unwrap_or_else(|| "other".to_owned())
        }
    };
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = state.as_mut() {
        let timeline = VIEW.load(Relaxed) == 3 || s.snap.is_some();
        for (name, tid, start, ns) in spans.drain(..) {
            let g = group(tid);
            if timeline {
                s.timeline.push_back((start, ns, group_index(&g), name));
            }
            if let Some(snap) = s.snap.as_mut()
                && start >= snap.started
                && start < snap.started + SNAP_NS
            {
                let e = snap.spans.entry((g.clone(), name)).or_insert((0, 0));
                e.0 += ns;
                e.1 += 1;
            }
            let e = s.spans.entry((g, name)).or_insert((0, 0));
            e.0 += ns;
            e.1 += 1;
        }
    } else {
        spans.clear();
    }
}

/// A present happened: the frame's length and the totals are taken, the
/// keys looked at. Called at every present.
pub(crate) fn frame() {
    let now = crate::clock::now_ns();
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = state.get_or_insert_with(|| State {
        frames: Default::default(),
        last_ns: now,
        totals: crate::framelog::totals(),
        sums: vec![0; T::Count as usize],
        mean: vec![0.0; T::Count as usize],
        spans: HashMap::new(),
        rows: Vec::new(),
        gpu: HashMap::new(),
        gpu_rows: Vec::new(),
        memory_rows: Vec::new(),
        memory_at: 0,
        summed: 0,
        since_ns: now,
        vram: (0, 0),
        keys: [false; 9],
        timeline: Default::default(),
        presents: Default::default(),
        laid: None,
        snap: None,
    });
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        // SAFETY: plain queries.
        let down = [0x78, 0x26, 0x28, 0x25, 0x27, 0x79, 0x77, 0x76, 0x70].map(|vk| unsafe { GetAsyncKeyState(vk) } < 0);
        let pressed = |k: usize| down[k] && !s.keys[k];
        let front = focused();
        // F9 the processor's view, F10 the card's, F8 the memory's, F7 the
        // time line: the same key again hides it, another key switches to
        // its view.
        for (key, view) in [(0, 0), (5, 1), (6, 2), (7, 3)] {
            if pressed(key) && front {
                if shown() && VIEW.load(Relaxed) == view {
                    SHOWN.store(false, Relaxed);
                } else {
                    VIEW.store(view, Relaxed);
                    SHOWN.store(true, Relaxed);
                    SCROLL.store(0, Relaxed);
                }
            }
        }
        if shown() && front {
            let at = SCROLL.load(Relaxed);
            if pressed(1) {
                SCROLL.store(at.saturating_sub(4), Relaxed);
            }
            if pressed(2) {
                SCROLL.store(at + 4, Relaxed);
            }
            let depth = DEPTH.load(Relaxed);
            if pressed(3) {
                DEPTH.store(depth.saturating_sub(1), Relaxed);
            }
            if pressed(4) {
                DEPTH.store((depth + 1).min(DEPTHS.len() as u32 - 1), Relaxed);
            }
        }
        // F1: a snapshot of every view to a file (and at the moments
        // `VICTORY_SNAPSHOT_AT` names, seconds since the start).
        if (pressed(8) && front || snapshot_due(now)) && s.snap.is_none() {
            s.snap = Some(Snap {
                started: now,
                totals: crate::framelog::totals(),
                frames: Vec::new(),
                spans: HashMap::new(),
                gpu: HashMap::new(),
            });
            RECORDING.store(true, Relaxed);
            tracing::info!("snapshot: recording {} s", SNAP_NS as f64 / 1e9);
        }
        s.keys = down;
    }
    if s.frames.len() == KEPT {
        s.frames.pop_front();
    }
    let frame_ms = (now - s.last_ns) as f32 / 1e6;
    s.frames.push_back(frame_ms);
    s.last_ns = now;
    if let Some(snap) = s.snap.as_mut() {
        if now < snap.started + SNAP_NS {
            snap.frames.push(frame_ms);
            if now + 30_000_000 >= snap.started + SNAP_NS {
                snap.totals = crate::framelog::totals()
                    .iter()
                    .zip(&snap.totals)
                    .map(|(now, was)| now.saturating_sub(*was))
                    .collect();
            }
        } else if now >= snap.started + SNAP_NS + SNAP_WAIT_NS {
            let snap = s.snap.take().expect("checked");
            RECORDING.store(false, Relaxed);
            let laid = lay_out(&s.timeline, &s.presents, now);
            write_snapshot(&snap, laid.as_ref(), s.vram);
        }
    }
    if (VIEW.load(Relaxed) == 3 && shown()) || s.snap.is_some() {
        s.presents.push_back(now);
        while s.presents.front().is_some_and(|&p| p + KEEP_NS < now) {
            s.presents.pop_front();
        }
        while s.timeline.front().is_some_and(|&(start, ..)| start + KEEP_NS < now) {
            s.timeline.pop_front();
        }
    } else if !s.timeline.is_empty() || !s.presents.is_empty() {
        s.timeline.clear();
        s.presents.clear();
        s.laid = None;
    }
    let totals = crate::framelog::totals();
    for (i, (now, was)) in totals.iter().zip(&s.totals).enumerate() {
        s.sums[i] += now.saturating_sub(*was);
    }
    s.totals = totals;
    s.summed += 1;
    if now - s.since_ns >= 500_000_000 {
        let n = f64::from(s.summed.max(1));
        for (m, sum) in s.mean.iter_mut().zip(&mut s.sums) {
            *m = *sum as f64 / n;
            *sum = 0;
        }
        s.rows = s
            .spans
            .drain()
            .map(|((group, name), (ns, count))| (group, name, ns as f64 / 1e6 / n, f64::from(count) / n))
            .collect();
        // Thread groups in a fixed order, the heaviest spans first in each.
        let order = |g: &str| match g {
            "title render thread" => 0,
            "title-main" => 1,
            "victory-submit" => 2,
            "victory-wide" => 3,
            "victory-relay" => 4,
            "card" => 5,
            _ => 6,
        };
        s.rows.sort_by(|a, b| order(&a.0).cmp(&order(&b.0)).then(a.0.cmp(&b.0)).then(b.2.total_cmp(&a.2)));
        s.gpu_rows = s.gpu.drain().map(|(name, (us, count))| (name, us as f64 / 1e3 / n, f64::from(count) / n)).collect();
        s.gpu_rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        s.summed = 0;
        s.since_ns = now;
        if VIEW.load(Relaxed) == 3 && shown() {
            s.laid = lay_out(&s.timeline, &s.presents, now);
        }
        #[cfg(feature = "diagnostics")]
        if shown() && VIEW.load(Relaxed) == 2 && now - s.memory_at >= 2_000_000_000 {
            s.memory_at = now;
            s.memory_rows = crate::copystat::memory_rows();
        }
        if shown()
            && let Some([budget, used, ..]) = crate::host::host().and_then(|h| h.video_memory())
        {
            s.vram = (used >> 20, budget >> 20);
        }
    }
}

/// The last frame that ended [`SHOW_BEHIND_NS`] ago or earlier, laid out for
/// the time line: the spans that overlap it, cut to it, each with its depth
/// among the spans of its thread group that hold it (a span inside a longer
/// one of the same thread goes a row lower).
fn lay_out(
    timeline: &std::collections::VecDeque<(u64, u64, u8, &'static str)>,
    presents: &std::collections::VecDeque<u64>,
    now: u64,
) -> Option<Laid> {
    let pairs: Vec<(u64, u64)> = presents.iter().zip(presents.iter().skip(1)).map(|(&a, &b)| (a, b)).collect();
    let &(from, to) = pairs.iter().rev().find(|(_, end)| end + SHOW_BEHIND_NS <= now)?;
    let mut spans: Vec<(u64, u64, u8, &'static str)> =
        timeline.iter().copied().filter(|&(start, len, ..)| start < to && start + len > from).collect();
    // Longer first among equal starts: a holder comes before what it holds.
    spans.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)).then(b.1.cmp(&a.1)));
    let ms = |ns: u64| ns as f64 / 1e6;
    let mut out = Vec::with_capacity(spans.len());
    let mut open: Vec<u64> = Vec::new();
    let mut group = u8::MAX;
    for (start, len, g, name) in spans {
        if g != group {
            (group, open) = (g, Vec::new());
        }
        while open.last().is_some_and(|&end| end <= start) {
            open.pop();
        }
        let depth = open.len().min(255) as u8;
        open.push(start + len);
        let (a, b) = (start.max(from), (start + len).min(to));
        out.push((ms(a - from), ms(b - a), g, depth, name));
    }
    // The title's own work on its render thread: where none of ours runs there
    // (the game's code between its calls into the layer), as spans of its own
    // so that the time line has no unexplained gaps.
    let mut ours: Vec<(f64, f64)> =
        out.iter().filter(|s| s.2 == 0 && s.3 == 0).map(|s| (s.0, s.0 + s.1)).collect();
    ours.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut at, end) = (0.0, ms(to - from));
    for (a, b) in ours {
        if a > at + 0.05 {
            out.push((at, a - at, 0, 0, "the title's own work"));
        }
        at = at.max(b);
    }
    if end > at + 0.05 {
        out.push((at, end - at, 0, 0, "the title's own work"));
    }
    Some(Laid { ms: end, spans: out })
}

/// A colour for span `name`, the same every frame.
fn colour_of(name: &str) -> u32 {
    const PALETTE: [u32; 8] = [0xffe0_6060, 0xffe0_a040, 0xffd0_d050, 0xff60_c070, 0xff50_b0e0, 0xff80_80f0, 0xffc0_70e0, 0xffa0_a0a0];
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

/// The time line (F7): a row per thread group and depth with the frame's spans
/// as bars on one time axis, the card's lists last; under it the spans of
/// the top two depths in the order they started, with their start, length
/// and share of the frame.
fn draw_timeline(px: &mut [u32], s: &State, lines: &mut Vec<(String, Option<(f64, f64, f64)>)>) -> u32 {
    const HEAD: u32 = 0xff9c_d0ff;
    const TEXT: u32 = 0xfff0_f0f0;
    text_at(px, 2, 22, "TIME LINE OF ONE FRAME, 2 S AGO (F9: TOTALS)", HEAD);
    let Some(laid) = &s.laid else {
        text_at(px, 2, 34, "(THE CARD'S TIMES ARRIVE A SECOND AND A HALF LATE)", TEXT);
        return 44;
    };
    let (x0, w) = (92u32, WIDTH - 96);
    let at = |ms: f64| x0 + ((ms / laid.ms.max(0.001)) * f64::from(w)) as u32;
    // ticks every 10 ms
    let mut y = 32;
    let mut t = 0.0;
    while t <= laid.ms {
        put(px, at(t), y, HEAD);
        put(px, at(t), y + 1, HEAD);
        t += 10.0;
    }
    text_at(px, 2, y, &format!("{:.0} MS", laid.ms), HEAD);
    y += 4;
    const BAR: u32 = 6;
    const DEPTHS_SHOWN: u8 = 3;
    for (g, name) in GROUPS.iter().enumerate() {
        let rows = laid.spans.iter().filter(|sp| sp.2 as usize == g).map(|sp| sp.3 + 1).max().unwrap_or(0).min(DEPTHS_SHOWN);
        if rows == 0 {
            continue;
        }
        let short: String = name.trim_start_matches("title ").trim_start_matches("victory-").chars().take(14).collect();
        text_at(px, 2, y, &short, HEAD);
        for sp in laid.spans.iter().filter(|sp| sp.2 as usize == g && sp.3 < rows) {
            let (a, b) = (at(sp.0), at(sp.0 + sp.1).max(at(sp.0) + 1));
            let top = y + u32::from(sp.3) * (BAR + 1);
            let c = colour_of(sp.4);
            for yy in top..top + BAR {
                for x in a..b.min(x0 + w) {
                    put(px, x, yy, c);
                }
            }
        }
        y += u32::from(rows) * (BAR + 1) + 3;
    }
    // In order: the spans of the top two depths that take a hundredth of the frame
    let mut listed: Vec<&(f64, f64, u8, u8, &'static str)> =
        laid.spans.iter().filter(|sp| sp.3 < 2 && sp.1 >= laid.ms / 100.0).collect();
    listed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for sp in listed {
        let group: String = GROUPS.get(sp.2 as usize).copied().unwrap_or("other")
            .trim_start_matches("title ").trim_start_matches("victory-").chars().take(6).collect();
        let indent = if sp.3 == 1 { "  " } else { "" };
        let what = if sp.4.starts_with('[') { pipeline_text(sp.4) } else { sp.4.to_owned() };
        lines.push((
            format!("{:6.1} {indent}{} {}", sp.0, group, what).to_uppercase(),
            Some((sp.1, sp.1 / laid.ms.max(0.001) * 100.0, laid.ms)),
        ));
    }
    y + 2
}

/// Whether a moment `VICTORY_SNAPSHOT_AT` names (seconds since the start,
/// comma-separated) has come: each once.
fn snapshot_due(now: u64) -> bool {
    static AT: std::sync::OnceLock<Mutex<Vec<f64>>> = std::sync::OnceLock::new();
    let at = AT.get_or_init(|| {
        let list = std::env::var("VICTORY_SNAPSHOT_AT").unwrap_or_default();
        Mutex::new(list.split(',').filter_map(|t| t.trim().parse().ok()).collect())
    });
    let mut at = at.lock().unwrap_or_else(|e| e.into_inner());
    let seconds = now as f64 / 1e9;
    match at.iter().position(|&t| t <= seconds) {
        Some(i) => {
            at.remove(i);
            true
        }
        None => false,
    }
}

/// Writes snapshot `snap` as Markdown to `snapshots/<seconds>.md` (relative to
/// the run's folder, where `run.sh` starts the title): frame times, the frame
/// log's totals, the processor's spans by thread group, the card's time by
/// pipeline, one frame's time line in order, and where the memory is.
fn write_snapshot(snap: &Snap, laid: Option<&Laid>, vram: (u64, u64)) {
    use std::fmt::Write;
    let n = snap.frames.len().max(1) as f64;
    let mut sorted = snap.frames.clone();
    sorted.sort_by(f32::total_cmp);
    let pick = |q: f64| sorted.get(((sorted.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
    let mean = snap.frames.iter().map(|&f| f64::from(f)).sum::<f64>() / n;
    let mut md = String::new();
    let seconds = snap.started as f64 / 1e9;
    let _ = writeln!(md, "# Snapshot at {seconds:.0} s, frame {}\n", crate::frame::current());
    let _ = writeln!(md, "{} frames in {:.1} s: mean {mean:.1} ms ({:.1} fps), median {:.1}, p90 {:.1}, max {:.1}. Card memory {} of {} MB.\n",
        snap.frames.len(), SNAP_NS as f64 / 1e9, 1000.0 / mean.max(0.001), pick(0.5), pick(0.9), pick(1.0), vram.0, vram.1);
    let per = |t: T| snap.totals.get(t as usize).copied().unwrap_or(0) as f64 / n;
    let _ = writeln!(md, "Per frame: card idle {:.1} ms ({:.1} behind us), up {:.1} MB, back {:.1} MB, present {:.1} ms.\n",
        per(T::CardIdleUs) / 1e3, per(T::CardIdleBehindUs) / 1e3, per(T::CopyBytes) / 1e6, per(T::CopyBackBytes) / 1e6, per(T::PresentUs) / 1e3);
    // processor
    let _ = writeln!(md, "## Processor: spans per frame by thread\n\n| thread | span | ms | count |\n|---|---|---:|---:|");
    let mut rows: Vec<(&String, &&'static str, f64, f64)> =
        snap.spans.iter().map(|((g, name), (ns, c))| (g, name, *ns as f64 / 1e6 / n, f64::from(*c) / n)).collect();
    rows.sort_by(|a, b| a.0.cmp(b.0).then(b.2.total_cmp(&a.2)));
    for (g, name, ms, count) in rows.iter().filter(|r| r.2 >= 0.05) {
        let _ = writeln!(md, "| {g} | {name} | {ms:.2} | {count:.1} |");
    }
    // card
    let all: f64 = snap.gpu.values().map(|v| v.0 as f64).sum::<f64>() / 1e3 / n;
    let _ = writeln!(md, "\n## Card: time per frame by pipeline ({all:.1} ms in all)\n\n| pipeline | ms | count |\n|---|---:|---:|");
    let mut gpu: Vec<(&String, f64, f64)> = snap.gpu.iter().map(|(k, (us, c))| (k, *us as f64 / 1e3 / n, f64::from(*c) / n)).collect();
    gpu.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (name, ms, count) in gpu.iter().take(60) {
        let text = pipeline_text(name).to_lowercase();
        let _ = writeln!(md, "| {text} `{name}` | {ms:.2} | {count:.1} |");
    }
    // time line
    if let Some(laid) = laid {
        let _ = writeln!(md, "\n## One frame in order ({:.1} ms)\n\nStart and length in ms from the frame's start; depth 1 is inside the span above it.\n\n| start | length | % | thread | depth | span |\n|---:|---:|---:|---|---:|---|", laid.ms);
        let mut spans: Vec<&(f64, f64, u8, u8, &'static str)> = laid.spans.iter().filter(|sp| sp.3 < 3 && sp.1 >= laid.ms / 200.0).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        for sp in spans {
            let group = GROUPS.get(sp.2 as usize).copied().unwrap_or("other");
            let what = if sp.4.starts_with('[') { format!("{} `{}`", pipeline_text(sp.4).to_lowercase(), sp.4) } else { sp.4.to_owned() };
            let _ = writeln!(md, "| {:.1} | {:.1} | {:.0} | {group} | {} | {what} |", sp.0, sp.1, sp.1 / laid.ms.max(0.001) * 100.0, sp.3);
        }
    }
    // memory
    #[cfg(feature = "diagnostics")]
    {
        let _ = writeln!(md, "\n## Memory\n\n| group | what | MB | count |\n|---|---|---:|---:|");
        for (g, what, bytes, count) in crate::copystat::memory_rows() {
            let _ = writeln!(md, "| {g} | {} | {:.0} | {count} |", what.trim(), bytes as f64 / f64::from(1u32 << 20));
        }
    }
    let dir = std::path::Path::new("snapshots");
    let file = dir.join(format!("{seconds:08.1}.md"));
    match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&file, md)) {
        Ok(()) => tracing::info!("snapshot written: {}", std::fs::canonicalize(&file).unwrap_or(file).display()),
        Err(e) => tracing::warn!(error = %e, "snapshot not written"),
    }
}

/// Whether the title should not see virtual key `vk`: an arrow key while
/// the overlay is up (they move through it).
pub fn takes_key(vk: u8) -> bool {
    shown() && (0x25..=0x28).contains(&vk)
}

fn focused() -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let mut process = 0;
    // SAFETY: plain queries; a null window gives process 0.
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut process));
        process == GetCurrentProcessId()
    }
}

/// The overlay as `WIDTH` x `HEIGHT` pixels of 0xAARRGGBB, rows top first;
/// alpha 0 where nothing is drawn.
pub(crate) fn draw() -> Vec<u32> {
    let mut px = vec![0u32; (WIDTH * HEIGHT) as usize];
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = state.as_ref() else { return px };
    let ms = |t: T| s.mean[t as usize] / 1e3;
    let recent: Vec<f32> = s.frames.iter().rev().take(15).copied().collect();
    let frame = f64::from(recent.iter().sum::<f32>() / recent.len().max(1) as f32);
    let mut sorted: Vec<f32> = s.frames.iter().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let pick = |q: f64| sorted.get(((sorted.len().max(1) - 1) as f64 * q) as usize).copied().unwrap_or(0.0);
    let fps = if frame > 0.0 { 1000.0 / frame } else { 0.0 };
    const TEXT: u32 = 0xfff0_f0f0;
    const HEAD: u32 = 0xff9c_d0ff;
    text_at(&mut px, 2, 2, &format!("FRAME {frame:.0} MS  {fps:.1} FPS  P90 {:.0}  MAX {:.0}", pick(0.9), pick(1.0)), TEXT);
    text_at(
        &mut px,
        2,
        12,
        &format!(
            "CARD IDLE {:.0} (BEHIND US {:.0})  UP {:.0} MB  BACK {:.0} MB  VRAM {}/{} MB",
            ms(T::CardIdleUs),
            ms(T::CardIdleBehindUs),
            s.mean[T::CopyBytes as usize] / 1e6,
            s.mean[T::CopyBackBytes as usize] / 1e6,
            s.vram.0,
            s.vram.1
        ),
        TEXT,
    );
    let depth = DEPTHS[DEPTH.load(Relaxed) as usize];
    let mut lines: Vec<(String, Option<(f64, f64, f64)>)> = Vec::new();
    let view = VIEW.load(Relaxed);
    let mut top = 32;
    if view == 3 {
        top = draw_timeline(&mut px, s, &mut lines);
    } else if view == 2 {
        // Where the memory is: each row a bar against the card's budget
        // (card rows) or the process's private bytes (the others).
        text_at(&mut px, 2, 22, "MEMORY (F9: PROCESSOR, F10: CARD)                   MB     N", HEAD);
        let budget = s.memory_rows.iter().find(|r| r.1.starts_with("budget")).map_or(1, |r| r.2.max(1));
        let private = s.memory_rows.iter().find(|r| r.1 == "process private").map_or(1, |r| r.2.max(1));
        let mut group = "";
        for (g, what, bytes, count) in &s.memory_rows {
            if *g != group {
                group = g;
                lines.push((g.to_uppercase(), None));
            }
            let of = if *g == "card memory" { budget } else { private };
            // (The bar's length is taken from the share; the numbers shown
            // are MB and the count.)
            let mb = |b: u64| b as f64 / f64::from(1u32 << 20);
            lines.push((what.to_uppercase(), Some((mb(*bytes), *count as f64, mb(of)))));
        }
        if s.memory_rows.is_empty() {
            lines.push(("(TAKEN EVERY TWO SECONDS)".to_owned(), None));
        }
    } else if view == 1 {
        // The card's view: its time by pipeline, heaviest first.
        text_at(&mut px, 2, 22, "CARD PER FRAME (F9: PROCESSOR)                      MS     X", HEAD);
        let all: f64 = s.gpu_rows.iter().map(|r| r.1).sum();
        lines.push((format!("ALL PIPELINES {all:.1} MS"), None));
        for (name, ms, count) in &s.gpu_rows {
            lines.push((pipeline_text(name), Some((*ms, *count, frame))));
        }
        if s.gpu_rows.is_empty() {
            lines.push(("(THE CARD'S TIMES ARRIVE A SECOND AND A HALF LATE)".to_owned(), None));
        }
    } else {
        text_at(&mut px, 2, 22, "SPAN PER FRAME (F10: CARD)                          MS     X", HEAD);
        // A heading when the thread group changes, then its spans.
        let (mut group, mut in_group) = ("", 0);
        for (g, name, ms, count) in &s.rows {
            if g != group {
                (group, in_group) = (g, 0);
                lines.push((g.to_uppercase(), None));
            }
            in_group += 1;
            if in_group <= depth {
                lines.push((name.to_uppercase(), Some((*ms, *count, frame))));
            }
        }
    }
    // (the time line's list has the rows left under its bars)
    let room = if view == 3 { ((HEIGHT - 12).saturating_sub(top) / ROW) as usize } else { ROWS };
    let first = (SCROLL.load(Relaxed) as usize).min(lines.len().saturating_sub(room));
    SCROLL.store(first as u32, Relaxed);
    // (the card's view: wider names, the shaders' own)
    let names = if view == 1 || view == 3 { 228 } else { 150 };
    let (bar_x, bar_w) = (names, WIDTH - names - 104);
    let mut y = top;
    for (label, value) in lines.iter().skip(first).take(room) {
        match value {
            None => text_at(&mut px, 2, y, label, HEAD),
            Some((v, count, full)) => {
                let chars = if view == 3 { 36 } else { 23 };
                text_at(&mut px, 8, y, &label.chars().take(chars).collect::<String>(), TEXT);
                let share = v / full.max(1.0);
                let len = (share.min(1.0) * f64::from(bar_w)) as u32;
                let colour = match share {
                    f if f >= 0.25 => 0xffe0_5050,
                    f if f >= 0.08 => 0xffe0_b040,
                    _ => 0xff50_c070,
                };
                for yy in y..y + 7 {
                    for x in bar_x..bar_x + len.max(1) {
                        put(&mut px, x, yy, colour);
                    }
                }
                let numbers = match view {
                    2 => format!("{v:7.0}{count:7.0}"),
                    // (the time line: the span's length and its share of the frame, %)
                    3 => format!("{v:6.1}{count:5.0}%"),
                    _ => format!("{v:6.1}{count:6.1}"),
                };
                text_at(&mut px, bar_x + bar_w + 4, y, &numbers, TEXT);
            }
        }
        y += ROW;
    }
    text_at(
        &mut px,
        2,
        y + 2,
        &format!("ROWS {}-{} OF {}  UP/DOWN SCROLL  LEFT/RIGHT DEPTH", first + 1, (first + room).min(lines.len()), lines.len()),
        HEAD,
    );
    outline(&mut px);
    px
}

/// A dark outline around every lit pixel, for reading over the game.
fn outline(px: &mut [u32]) {
    let lit: Vec<bool> = px.iter().map(|p| p >> 24 != 0).collect();
    let at = |x: i32, y: i32| x >= 0 && y >= 0 && (x as u32) < WIDTH && (y as u32) < HEIGHT && lit[(y as u32 * WIDTH + x as u32) as usize];
    for y in 0..HEIGHT as i32 {
        for x in 0..WIDTH as i32 {
            if !at(x, y) && (-1..=1).any(|dy| (-1..=1).any(|dx| at(x + dx, y + dy))) {
                px[(y as u32 * WIDTH + x as u32) as usize] = 0xff00_0000;
            }
        }
    }
}

fn put(px: &mut [u32], x: u32, y: u32, colour: u32) {
    if x < WIDTH && y < HEIGHT {
        px[(y * WIDTH + x) as usize] = colour;
    }
}

fn text_at(px: &mut [u32], x: u32, y: u32, text: &str, colour: u32) {
    for (i, c) in text.chars().enumerate() {
        let rows = glyph(c.to_ascii_uppercase());
        for (dy, row) in rows.iter().enumerate() {
            for dx in 0..5 {
                if row >> (4 - dx) & 1 != 0 {
                    put(px, x + i as u32 * 6 + dx, y + dy as u32, colour);
                }
            }
        }
    }
}

/// A character of a 5x7 font: seven rows, the leftmost pixel the highest
/// of five bits. Characters it lacks are blank.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [28, 18, 17, 17, 17, 18, 28],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 17, 10, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        '/' => [0, 1, 2, 4, 8, 16, 0],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '%' => [24, 25, 2, 4, 8, 19, 3],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        _ => [0; 7],
    }
}

/// How many back-buffer pixels an overlay pixel takes for a back buffer
/// `height` rows tall: 3 at 1080p, 4 at 1440p, 6 at 2160p;
/// `VICTORY_OVERLAY_SCALE` sets it (1 to 8).
pub(crate) fn scale(height: u32) -> u32 {
    let set = std::env::var("VICTORY_OVERLAY_SCALE").ok().and_then(|v| v.trim().parse::<u32>().ok());
    set.unwrap_or(height.div_ceil(360)).clamp(1, 8)
}

/// The overlay at scale `k` in the back buffer's format (4 bytes a pixel
/// for 8-bit RGBA or BGRA and 10-bit RGB; `None` for other formats), and
/// its lit runs at scale 1 (row, first column, columns): only those are
/// copied, so nothing hides the game around them.
pub(crate) fn pixels(format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, k: u32) -> Option<(Vec<u32>, Vec<(u32, u32, u32)>)> {
    use windows::Win32::Graphics::Dxgi::Common::*;
    let convert: fn(u32) -> u32 = match format {
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => |p| p,
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => |p| {
            let (r, g, b) = (p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff);
            0xff00_0000 | b << 16 | g << 8 | r
        },
        DXGI_FORMAT_R10G10B10A2_UNORM => |p| {
            let (r, g, b) = (p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff);
            let ten = |c: u32| c * 1023 / 255;
            3 << 30 | ten(b) << 20 | ten(g) << 10 | ten(r)
        },
        _ => return None,
    };
    let base = draw();
    let mut runs = Vec::new();
    for y in 0..HEIGHT {
        let row = &base[(y * WIDTH) as usize..((y + 1) * WIDTH) as usize];
        let mut x = 0;
        while x < WIDTH {
            if row[x as usize] >> 24 == 0 {
                x += 1;
                continue;
            }
            let from = x;
            while x < WIDTH && row[x as usize] >> 24 != 0 {
                x += 1;
            }
            runs.push((y, from, x - from));
        }
    }
    let w = WIDTH * k;
    let mut out = vec![0u32; (w * HEIGHT * k) as usize];
    for y in 0..HEIGHT * k {
        for x in 0..w {
            out[(y * w + x) as usize] = convert(base[((y / k) * WIDTH + x / k) as usize]);
        }
    }
    Some((out, runs))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_overlay_draws_lit_runs_only() {
        super::frame();
        super::span("submit", 1, 0, 2_000_000);
        super::frame();
        let px = super::draw();
        assert_eq!(px.len(), (super::WIDTH * super::HEIGHT) as usize);
        assert!(px.iter().any(|&p| p == 0xfff0_f0f0), "some text");
        assert!(px.iter().any(|&p| p == 0), "the game shows through");
        let (_, runs) = super::pixels(windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM, 2).unwrap();
        assert!(!runs.is_empty() && runs.iter().all(|r| r.2 > 0));
        for c in "0123456789FPSMS".chars() {
            assert!(super::glyph(c).iter().any(|&r| r != 0), "{c}");
        }
    }
}

#[cfg(test)]
mod timeline_tests {
    #[test]
    fn the_time_line_shows_the_frame_two_seconds_back_with_nested_spans_a_row_lower() {
        const MS: u64 = 1_000_000;
        let now = 10_000 * MS;
        // presents at 7,900, 8,000 and 8,100 ms: the frame shown is 7,900..8,000 (the
        // last that ended two seconds ago); 8,000..8,100 is too recent
        let presents = [7_900 * MS, 8_000 * MS, 8_100 * MS].into_iter().collect();
        let timeline = [
            // submit 7,910..7,960 with copies inside it, on group 2
            (7_910 * MS, 50 * MS, 2, "submit"),
            (7_920 * MS, 10 * MS, 2, "copies"),
            // a card list across the frame's start: cut to it
            (7_890 * MS, 30 * MS, 5, "list"),
            // after the frame: left out
            (8_050 * MS, 5 * MS, 2, "submit"),
        ]
        .into_iter()
        .collect();
        let laid = super::lay_out(&timeline, &presents, now).expect("a frame");
        assert_eq!(laid.ms, 100.0);
        let find = |name| laid.spans.iter().find(|s| s.4 == name).copied().unwrap();
        // (and the title's own work filling its render thread, which has none of ours here)
        assert_eq!(laid.spans.len(), 4);
        assert_eq!(find("the title's own work").1, 100.0);
                assert_eq!(find("submit").3, 0);
        assert_eq!(find("copies").3, 1);
        assert_eq!((find("copies").0, find("copies").1), (20.0, 10.0));
        assert_eq!((find("list").0, find("list").1), (0.0, 20.0));
    }
}
