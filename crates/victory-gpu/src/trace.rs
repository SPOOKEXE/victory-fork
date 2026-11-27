//! The frame graph: timed spans of the layer's threads and the card's own
//! list times, written as a Chrome trace (JSON) compressed with gzip, to
//! open in ui.perfetto.dev or chrome://tracing.
//!
//! `VICTORY_TRACE=<file>` turns it on (`run.sh` sets it). Events gather in
//! memory and a thread of its own writes them once a second, each second
//! a gzip member of its own appended to the file: a run that freezes or
//! dies keeps everything up to the last second. (`gzip -dc` reads the
//! members as one; `run.sh` repacks them into one member at the end, which
//! every viewer reads.) `VICTORY_TRACE_MB` (1,024) caps the file.
//!
//! What is timed: the title's present and submit calls, our submissions'
//! steps (refreshes, heap sync, card copies), the copy-back thread's waits
//! and landings, settles and sweeps; per frame a marker and counters; on a
//! track of its own the card's time per list (`crate::cardmeter`, which is
//! on whenever this is).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

enum Event {
    /// A span of a thread: name, thread, start and length (ns), a number.
    Span(&'static str, u32, u64, u64, u64),
    /// A moment of a thread, with a number.
    Mark(&'static str, u32, u64, u64),
    /// A counter's value at a moment.
    Count(&'static str, u64, f64),
    /// A span of the card: name, start and length (ns, our clock).
    Card(&'static str, u64, u64),
    /// A thread's name, once.
    Thread(u32, String),
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
/// Compressed bytes written so far.
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static FULL: AtomicBool = AtomicBool::new(false);

/// Where the trace goes, if it is on.
fn path() -> Option<&'static std::path::PathBuf> {
    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = std::path::PathBuf::from(std::env::var_os("VICTORY_TRACE").filter(|v| !v.is_empty())?);
        std::thread::Builder::new().name("victory-trace".into()).spawn(writer).ok()?;
        tracing::info!("frame graph: {} (Chrome trace, gzip; open in ui.perfetto.dev)", path.display());
        Some(path)
    })
    .as_ref()
}

/// Whether the frame graph is written to its file.
fn file() -> bool {
    path().is_some()
}

/// Whether spans are timed: for the file, or for the overlay while it is
/// up (`crate::overlay`, which sums them by name).
pub(crate) fn on() -> bool {
    #[cfg(windows)]
    if crate::overlay::shown() || crate::overlay::recording() {
        return true;
    }
    file()
}

fn thread() -> u32 {
    crate::fast_local! {
        static ID: u32 = {
            #[cfg(windows)]
            // SAFETY: a plain query.
            let id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            #[cfg(not(windows))]
            let id = 0;
            let name = std::thread::current().name().map_or_else(|| format!("thread {id}"), str::to_owned);
            #[cfg(windows)]
            crate::overlay::thread_name(id, &name);
            push(Event::Thread(id, name));
            id
        };
    }
    ID.with(|id| *id)
}

/// Events waiting for the writer at most; past it new ones are dropped
/// ([`DROPPED`]). The writer formats and packs what is waiting once a
/// second: with more than it can pack in a second the next batch was
/// bigger, the one after bigger still (a run with spans per table entry
/// held 23 GB of them).
const WAITING_MOST: usize = 1_000_000;

/// Events dropped because [`WAITING_MOST`] were waiting, since the start.
static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn push(event: Event) {
    if FULL.load(Relaxed) || !file() {
        return;
    }
    let mut events = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    if events.len() >= WAITING_MOST {
        DROPPED.fetch_add(1, Relaxed);
        return;
    }
    events.push(event);
}

/// A span being timed; recorded when dropped.
pub(crate) struct Span {
    name: &'static str,
    at: u64,
    arg: u64,
}

impl Span {
    /// A number shown with the span (bytes, count).
    pub(crate) fn arg(&mut self, arg: u64) {
        self.arg = arg;
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let end = crate::clock::now_ns();
        let tid = thread();
        #[cfg(windows)]
        crate::overlay::span(self.name, tid, self.at, end.saturating_sub(self.at));
        push(Event::Span(self.name, tid, self.at, end.saturating_sub(self.at), self.arg));
    }
}

/// This thread's track and the moment, if spans are timed (0, 0 if not):
/// for [`waited`].
pub(crate) fn asked() -> (u32, u64) {
    if on() { (thread(), crate::clock::now_ns()) } else { (0, 0) }
}

/// A span `name` on track `tid` from `at` ([`asked`]) to now, recorded by
/// another thread: what a thread waited for until something else came.
pub(crate) fn waited(name: &'static str, tid: u32, at: u64) {
    if at == 0 || !on() {
        return;
    }
    let len = crate::clock::now_ns().saturating_sub(at);
    #[cfg(windows)]
    crate::overlay::span(name, tid, at, len);
    push(Event::Span(name, tid, at, len, 0));
}

/// A span `name` on the calling thread's track from `at` (a [`crate::clock`]
/// reading, `now_ns`) to now: for timings made outside this crate (the title's
/// calls into the runtime, `victory-hle`'s import timing).
pub fn timed(name: &'static str, at: u64) {
    if !on() {
        return;
    }
    let len = crate::clock::now_ns().saturating_sub(at);
    let tid = thread();
    #[cfg(windows)]
    crate::overlay::span(name, tid, at, len);
    push(Event::Span(name, tid, at, len, 0));
}

/// Whether spans are timed now ([`on`]), for timings made outside this crate.
pub fn timing() -> bool {
    on()
}

/// Times the caller's scope as `name` on this thread's track.
pub(crate) fn span(name: &'static str) -> Option<Span> {
    on().then(|| Span { name, at: crate::clock::now_ns(), arg: 0 })
}

/// A moment on this thread's track.
pub(crate) fn mark(name: &'static str, arg: u64) {
    if on() {
        push(Event::Mark(name, thread(), crate::clock::now_ns(), arg));
    }
}

/// A counter's value now.
pub(crate) fn count(name: &'static str, value: f64) {
    if on() {
        push(Event::Count(name, crate::clock::now_ns(), value));
    }
}

/// The card ran `name` from `at` for `len` (ns of [`crate::clock`]).
pub(crate) fn card(name: &'static str, at: u64, len: u64) {
    if on() {
        #[cfg(windows)]
        crate::overlay::span(name, CARD_TRACK, at, len);
        push(Event::Card(name, at, len));
    }
}

/// The thread number the card's spans are summed under in the overlay.
pub(crate) const CARD_TRACK: u32 = u32::MAX;

/// A frame ends (the title's present returned): a marker with its number,
/// the frame's length, and every 30 frames the card's memory in use.
pub(crate) fn frame() {
    if !on() {
        return;
    }
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = crate::clock::now_ns();
    let last = LAST.swap(now, Relaxed);
    let frame = crate::frame::current();
    mark("frame", frame);
    if last != 0 {
        count("frame ms", (now - last) as f64 / 1e6);
    }
    #[cfg(windows)]
    if frame.is_multiple_of(30)
        && let Some([budget, used, ..]) = crate::host::host().and_then(|h| h.video_memory())
    {
        count("card MB in use", (used >> 20) as f64);
        count("card MB budget", (budget >> 20) as f64);
    }
}

/// Writes what was gathered now (the writer, once a second).
fn flush() {
    let Some(path) = path() else {
        return;
    };
    static NAMED: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());
    let mut named = NAMED.lock().unwrap_or_else(|e| e.into_inner());
    let events = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()));
    let first = WRITTEN.load(Relaxed) == 0;
    if events.is_empty() && !first {
        return;
    }
    static SAID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dropped = DROPPED.load(Relaxed);
    if dropped > SAID.swap(dropped, Relaxed) {
        tracing::warn!(dropped, "frame graph: events dropped since the start, more were waiting than the writer keeps up with");
    }
    let text = json(&events, first, &mut named);
    let packed = gzip(text.as_bytes());
    let most = std::env::var("VICTORY_TRACE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(1024) << 20;
    if WRITTEN.fetch_add(packed.len() as u64, Relaxed) + packed.len() as u64 > most {
        FULL.store(true, Relaxed);
        tracing::warn!("frame graph: VICTORY_TRACE_MB reached, nothing more is recorded");
        return;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
    if let Err(e) = file.and_then(|mut f| f.write_all(&packed)) {
        FULL.store(true, Relaxed);
        tracing::warn!(error = %e, "frame graph not written");
    }
}

fn writer() {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        flush();
    }
}

/// The events as Chrome trace events: each followed by a comma (the array
/// may stay open: the format allows it), the first chunk with the array's
/// opening and the track names.
fn json(events: &[Event], first: bool, named: &mut BTreeSet<u32>) -> String {
    let us = |ns: u64| ns as f64 / 1e3;
    let mut s = String::with_capacity(events.len() * 96 + 256);
    if first {
        s.push_str("[\n");
        s.push_str(r#"{"name":"process_name","ph":"M","pid":1,"args":{"name":"VIctory"}},"#);
        s.push('\n');
        s.push_str(r#"{"name":"process_name","ph":"M","pid":2,"args":{"name":"card"}},"#);
        s.push('\n');
        s.push_str(r#"{"name":"thread_name","ph":"M","pid":2,"tid":1,"args":{"name":"lists (card clock)"}},"#);
        s.push('\n');
    }
    for event in events {
        let _ = match event {
            Event::Span(name, tid, at, len, arg) => writeln!(
                s,
                r#"{{"name":"{name}","ph":"X","pid":1,"tid":{tid},"ts":{:.3},"dur":{:.3},"args":{{"n":{arg}}}}},"#,
                us(*at),
                us(*len)
            ),
            Event::Mark(name, tid, at, arg) => writeln!(
                s,
                r#"{{"name":"{name}","ph":"i","s":"g","pid":1,"tid":{tid},"ts":{:.3},"args":{{"n":{arg}}}}},"#,
                us(*at)
            ),
            Event::Count(name, at, value) => writeln!(
                s,
                r#"{{"name":"{name}","ph":"C","pid":1,"ts":{:.3},"args":{{"value":{value}}}}},"#,
                us(*at)
            ),
            Event::Card(name, at, len) => writeln!(
                s,
                r#"{{"name":"{name}","ph":"X","pid":2,"tid":1,"ts":{:.3},"dur":{:.3}}},"#,
                us(*at),
                us(*len)
            ),
            Event::Thread(tid, name) => {
                if !named.insert(*tid) {
                    continue;
                }
                let name: String = name.chars().filter(|c| *c != '"' && *c != '\\').collect();
                writeln!(s, r#"{{"name":"thread_name","ph":"M","pid":1,"tid":{tid},"args":{{"name":"{name}"}}}},"#)
            }
        };
    }
    s
}

/// One whole gzip member of `bytes`.
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; zlib_rs::compress_bound(bytes.len()) + 64];
    // 16 + 15: a gzip wrapper around a 32 KiB window.
    let config = zlib_rs::DeflateConfig { window_bits: 31, ..zlib_rs::DeflateConfig::new(6) };
    let (packed, rc) = zlib_rs::compress_slice(&mut out, bytes, config);
    if rc != zlib_rs::ReturnCode::Ok {
        return Vec::new();
    }
    let n = packed.len();
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_chrome_trace_lines_and_names_go_once() {
        let mut named = BTreeSet::new();
        let events = [
            Event::Thread(7, "victory-submit".into()),
            Event::Span("submit", 7, 2_000, 1_500, 3),
            Event::Thread(7, "victory-submit".into()),
            Event::Mark("frame", 7, 9_000, 42),
            Event::Count("fps", 9_000, 10.5),
            Event::Card("lists", 4_000, 500),
        ];
        let text = json(&events, true, &mut named);
        assert!(text.starts_with("[\n"));
        assert_eq!(text.matches(r#""thread_name","ph":"M","pid":1,"tid":7"#).count(), 1);
        assert!(text.contains(r#"{"name":"submit","ph":"X","pid":1,"tid":7,"ts":2.000,"dur":1.500,"args":{"n":3}},"#));
        assert!(text.contains(r#""name":"frame","ph":"i""#));
        assert!(text.contains(r#""name":"fps","ph":"C","pid":1,"ts":9.000,"args":{"value":10.5}"#));
        assert!(text.contains(r#""name":"lists","ph":"X","pid":2,"tid":1,"ts":4.000,"dur":0.500"#));
        // A later chunk has no opening.
        assert!(!json(&[], false, &mut named).starts_with('['));
    }

    #[test]
    fn a_member_is_gzip() {
        let packed = gzip(b"hello hello hello hello");
        assert_eq!(&packed[..2], &[0x1f, 0x8b]);
        let mut out = vec![0u8; 64];
        let config = zlib_rs::InflateConfig { window_bits: 31 };
        let (plain, rc) = zlib_rs::decompress_slice(&mut out, &packed, config);
        assert_eq!(rc, zlib_rs::ReturnCode::Ok);
        assert_eq!(plain, b"hello hello hello hello");
    }
}
