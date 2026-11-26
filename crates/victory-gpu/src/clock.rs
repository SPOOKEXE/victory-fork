//! A cheap clock for hot paths: the processor's time-stamp counter,
//! scaled once against `Instant`.
//!
//! Under Wine every `Instant::now` is a call into the Unix side (a few
//! hundred nanoseconds); the submitting thread stamped each of some
//! 13,000 copies a frame with it (7 % of that thread). Calibration spins
//! 10 ms at the first call. Without a time-stamp counter it is `Instant`.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(target_arch = "x86_64")]
fn ticks() -> u64 {
    // SAFETY: rdtsc has no preconditions on x86_64.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The epoch, in ticks, as an `Instant` and as a reading of the system's
/// performance counter (with its frequency), and ticks per nanosecond.
struct Scale {
    ticks: u64,
    at: Instant,
    per_ns: f64,
    qpc: (u64, u64),
}

/// The system's performance counter and its frequency.
fn qpc() -> (u64, u64) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
        let (mut count, mut frequency) = (0i64, 0i64);
        // SAFETY: plain queries into our integers.
        unsafe {
            QueryPerformanceCounter(&mut count);
            QueryPerformanceFrequency(&mut frequency);
        }
        (count as u64, frequency as u64)
    }
    #[cfg(not(windows))]
    (0, 0)
}

fn scale() -> &'static Scale {
    static SCALE: OnceLock<Scale> = OnceLock::new();
    SCALE.get_or_init(|| {
        let at = Instant::now();
        let qpc = qpc();
        #[cfg(target_arch = "x86_64")]
        {
            let first = ticks();
            while at.elapsed() < Duration::from_millis(10) {
                std::hint::spin_loop();
            }
            let (last, took) = (ticks(), at.elapsed());
            Scale { ticks: first, at, per_ns: (last - first) as f64 / took.as_nanos() as f64, qpc }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Scale { ticks: 0, at, per_ns: 0.0, qpc }
    })
}

/// Nanoseconds since the first call to this module.
pub fn now_ns() -> u64 {
    let s = scale();
    #[cfg(target_arch = "x86_64")]
    if s.per_ns > 0.0 {
        return (ticks().saturating_sub(s.ticks) as f64 / s.per_ns) as u64;
    }
    s.at.elapsed().as_nanos() as u64
}

/// A reading of the system's performance counter (the one the card's
/// clock calibration gives) on this clock, in nanoseconds.
pub(crate) fn from_qpc(count: u64) -> u64 {
    let (at, frequency) = scale().qpc;
    if frequency == 0 {
        return 0;
    }
    ((count.saturating_sub(at)) as u128 * 1_000_000_000 / u128::from(frequency)) as u64
}

/// Microseconds since the first call to this module.
pub(crate) fn now_us() -> u64 {
    now_ns() / 1_000
}

/// Milliseconds since the first call to this module.
pub(crate) fn now_ms() -> u64 {
    now_ns() / 1_000_000
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_keeps_time_with_instant() {
        let (a, at) = (super::now_ns(), std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let (b, took) = (super::now_ns(), at.elapsed().as_nanos() as f64);
        let ours = (b - a) as f64;
        assert!((ours - took).abs() < took * 0.05, "{ours} ns against {took}");
    }
}
