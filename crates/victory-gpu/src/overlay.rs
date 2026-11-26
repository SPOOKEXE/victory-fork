//! The in-game overlay (F9): what a frame's time goes to, span by span,
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

pub(crate) const WIDTH: u32 = 460;
pub(crate) const HEIGHT: u32 = 300;
/// Frames kept for the percentiles.
const KEPT: usize = 240;
/// Rows shown at once (Up and Down move through the rest).
const ROWS: usize = 28;
const ROW: u32 = 9;
/// Spans shown per thread at each depth (Left and Right).
const DEPTHS: [usize; 4] = [1, 3, 6, usize::MAX];

static SHOWN: AtomicBool = AtomicBool::new(false);
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
    summed: u32,
    since_ns: u64,
    vram: (u64, u64),
    /// F9, Up, Down, Left, Right as last seen.
    keys: [bool; 5],
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

/// A thread's name, once, as it first times a span.
pub(crate) fn thread_name(tid: u32, name: &str) {
    let group = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ');
    let group = if group == "thread" { "title render thread" } else { group };
    THREADS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(tid, group.to_owned());
}

/// A span ended: `ns` long, on thread `tid` (`crate::trace::CARD_TRACK`
/// for the card's).
pub(crate) fn span(name: &'static str, tid: u32, ns: u64) {
    let group = if tid == crate::trace::CARD_TRACK {
        "card".to_owned()
    } else {
        THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|t| t.get(&tid).cloned())
            .unwrap_or_else(|| "other".to_owned())
    };
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = state.as_mut() {
        let e = s.spans.entry((group, name)).or_insert((0, 0));
        e.0 += ns;
        e.1 += 1;
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
        summed: 0,
        since_ns: now,
        vram: (0, 0),
        keys: [false; 5],
    });
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        // SAFETY: plain queries.
        let down = [0x78, 0x26, 0x28, 0x25, 0x27].map(|vk| unsafe { GetAsyncKeyState(vk) } < 0);
        let pressed = |k: usize| down[k] && !s.keys[k];
        let front = focused();
        if pressed(0) && front {
            SHOWN.fetch_xor(true, Relaxed);
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
        s.keys = down;
    }
    if s.frames.len() == KEPT {
        s.frames.pop_front();
    }
    s.frames.push_back((now - s.last_ns) as f32 / 1e6);
    s.last_ns = now;
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
        s.summed = 0;
        s.since_ns = now;
        if shown()
            && let Some([budget, used, ..]) = crate::host::host().and_then(|h| h.video_memory())
        {
            s.vram = (used >> 20, budget >> 20);
        }
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
    text_at(&mut px, 2, 22, "SPAN PER FRAME                                      MS     X", HEAD);
    // A heading when the thread group changes, then its spans.
    let mut lines: Vec<(String, Option<(f64, f64)>)> = Vec::new();
    let (mut group, mut in_group) = ("", 0);
    for (g, name, ms, count) in &s.rows {
        if g != group {
            (group, in_group) = (g, 0);
            lines.push((g.to_uppercase(), None));
        }
        in_group += 1;
        if in_group <= depth {
            lines.push((name.to_uppercase(), Some((*ms, *count))));
        }
    }
    let first = (SCROLL.load(Relaxed) as usize).min(lines.len().saturating_sub(ROWS));
    SCROLL.store(first as u32, Relaxed);
    let (bar_x, bar_w) = (150u32, WIDTH - 150 - 86);
    let mut y = 32;
    for (label, value) in lines.iter().skip(first).take(ROWS) {
        match value {
            None => text_at(&mut px, 2, y, label, HEAD),
            Some((v, count)) => {
                text_at(&mut px, 8, y, &label.chars().take(23).collect::<String>(), TEXT);
                let share = v / frame.max(1.0);
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
                text_at(&mut px, bar_x + bar_w + 4, y, &format!("{v:6.1}{count:6.1}"), TEXT);
            }
        }
        y += ROW;
    }
    text_at(
        &mut px,
        2,
        y + 2,
        &format!("ROWS {}-{} OF {}  UP/DOWN SCROLL  LEFT/RIGHT DEPTH", first + 1, (first + ROWS).min(lines.len()), lines.len()),
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
        super::span("submit", 1, 2_000_000);
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
