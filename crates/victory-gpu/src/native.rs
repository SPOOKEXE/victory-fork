//! Title page views and memory calls made by the host system itself when the
//! layer runs under Wine on Linux.
//!
//! Wine's server keeps the views of a process in one list and walks it at
//! every map and unmap. The title shows some 40,000 pages and maps thousands
//! of them anew every frame: the server spent all of its one thread in that
//! walk and everything else that needs it (most memory and handle calls)
//! stood in line behind, seconds a frame. Here the title's physical pages are
//! a memory file of the host, and a view is an `mmap` of it over the
//! placeholder Wine reserved: under a microsecond, and the server never hears
//! of it. Wine then takes the whole placeholder for reserved memory, so every
//! question about such memory ([`query`]) and every change of its protection
//! ([`protect`]) has to come through this module.
//!
//! The calls go through a builtin library of ours (`native/`, built and put
//! on `WINEDLLPATH` by `run.sh`); without it, or off Wine, [`on`] is false
//! and everything is the Windows API's as before. `VICTORY_NATIVE=0`: off.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{OnceLock, RwLock};

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_MAPPED, MEM_PRIVATE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_NOACCESS, PAGE_READONLY,
    PAGE_READWRITE, VirtualProtect, VirtualQuery,
};

const K64: usize = 64 << 10;

// Linux, x86-64.
const MMAP: i64 = 9;
const MPROTECT: i64 = 10;
const MUNMAP: i64 = 11;
const CLOSE: i64 = 3;
const GETPID: i64 = 39;
const FTRUNCATE: i64 = 77;
const READV: i64 = 310;
const WRITEV: i64 = 311;
const MEMFD_CREATE: i64 = 319;
const PROT_READ: i64 = 1;
const PROT_READ_WRITE: i64 = 3;
const MAP_SHARED: i64 = 0x01;
const MAP_PRIVATE_ANON: i64 = 0x22;
const MAP_FIXED: i64 = 0x10;
const MAP_NORESERVE: i64 = 0x4000;

type Dispatch = unsafe extern "system" fn(u64, u32, *mut c_void) -> i32;

struct Native {
    dispatch: Dispatch,
    library: u64,
    pid: i64,
}

fn native() -> Option<&'static Native> {
    static NATIVE: OnceLock<Option<Native>> = OnceLock::new();
    NATIVE.get_or_init(load).as_ref()
}

fn load() -> Option<Native> {
    type Query = unsafe extern "system" fn(isize, *const c_void, i32, *mut u64, usize, *mut usize) -> i32;
    /// Wine's `MemoryWineUnixFuncs`: a builtin library's unix side.
    const UNIX_FUNCTIONS: i32 = 1000;
    if std::env::var_os("VICTORY_NATIVE").is_some_and(|v| v == "0") {
        return None;
    }
    // SAFETY: lookups by name; the routines have Wine's documented types.
    let native = unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        let at = GetProcAddress(ntdll, c"__wine_unix_call_dispatcher".as_ptr().cast())?;
        let dispatch = *(at as *const Option<Dispatch>);
        let query: Query = std::mem::transmute(GetProcAddress(ntdll, c"NtQueryVirtualMemory".as_ptr().cast())?);
        let name = std::env::var("VICTORY_NATIVE_LIBRARY").unwrap_or_else(|_| "victory_native.dll".into());
        let name = std::ffi::CString::new(name).ok()?;
        let module = LoadLibraryA(name.as_ptr().cast());
        let mut library = 0u64;
        if module.is_null() || query(-1, module.cast_const().cast(), UNIX_FUNCTIONS, &mut library, 8, std::ptr::null_mut()) < 0 {
            tracing::warn!("no native library beside Wine: every page view goes through its server");
            return None;
        }
        Native { dispatch: dispatch?, library, pid: 0 }
    };
    let native = Native { pid: native.call(GETPID, [0; 6]), ..native };
    // A debugger may attach to a diagnostics build that stands still.
    #[cfg(feature = "diagnostics")]
    {
        const PRCTL: i64 = 157;
        const SET_PTRACER: i64 = 0x5961_6d61;
        native.call(PRCTL, [SET_PTRACER, -1, 0, 0, 0, 0]);
    }
    tracing::info!("title page views are the host system's own");
    (native.pid > 0).then_some(native)
}

impl Native {
    /// A system call; its result, or the error number negated.
    fn call(&self, number: i64, x: [i64; 6]) -> i64 {
        let mut a = [number, x[0], x[1], x[2], x[3], x[4], x[5], -1];
        // SAFETY: the library's only routine, with the record it takes.
        unsafe { (self.dispatch)(self.library, 0, a.as_mut_ptr().cast()) };
        a[7]
    }
}

/// Whether the host system's own calls are there.
pub fn on() -> bool {
    native().is_some()
}

/// A title allocation of physical pages: a memory file, and a view of all of
/// it for the runtime's own reads and writes.
pub struct Pool {
    fd: i64,
    size: usize,
    all: usize,
}

impl Pool {
    pub fn new(size: usize) -> Option<Pool> {
        let n = native()?;
        let fd = n.call(MEMFD_CREATE, [c"victory-pages".as_ptr() as i64, 1, 0, 0, 0, 0]);
        if fd < 0 {
            return None;
        }
        let mut pool = Pool { fd, size, all: 0 };
        if n.call(FTRUNCATE, [fd, size as i64, 0, 0, 0, 0]) < 0 {
            return None;
        }
        let all = n.call(MMAP, [0, size as i64, PROT_READ_WRITE, MAP_SHARED, fd, 0]);
        if all < 0 {
            return None;
        }
        pool.all = all as usize;
        Some(pool)
    }

    /// Where byte `offset` of the pool is in the runtime's own view.
    pub fn at(&self, offset: usize) -> *mut u8 {
        (self.all + offset) as *mut u8
    }

    /// A key for the pool among the GPU side's sections.
    pub fn key(&self) -> usize {
        self.all
    }

    /// Shows `[offset, offset + len)` of the pool at `at`, in placeholder
    /// memory [`placeholder`] was told of, in place of what is there.
    pub fn show(&self, at: usize, len: usize, offset: usize) -> bool {
        let Some(n) = native() else { return false };
        let done = n.call(MMAP, [at as i64, len as i64, PROT_READ_WRITE, MAP_SHARED | MAP_FIXED, self.fd, offset as i64]);
        if done != at as i64 {
            return false;
        }
        note(at, len, true);
        true
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if let Some(n) = native() {
            if self.all != 0 {
                n.call(MUNMAP, [self.all as i64, self.size as i64, 0, 0, 0, 0]);
            }
            n.call(CLOSE, [self.fd, 0, 0, 0, 0, 0]);
        }
    }
}

/// Takes what is shown at `[at, at + len)` away again.
pub fn hide(at: usize, len: usize) {
    if let Some(n) = native() {
        note(at, len, false);
        n.call(MMAP, [at as i64, len as i64, 0, MAP_PRIVATE_ANON | MAP_FIXED | MAP_NORESERVE, -1, 0]);
    }
}

/// A placeholder the title reserved, and which of its 64 KiB blocks show
/// pages.
struct Range {
    base: usize,
    end: usize,
    shown: Vec<AtomicU64>,
}

static RANGES: RwLock<Vec<Range>> = RwLock::new(Vec::new());

fn ranges() -> std::sync::RwLockReadGuard<'static, Vec<Range>> {
    RANGES.read().unwrap_or_else(|e| e.into_inner())
}

/// The title reserved `[base, base + len)` for page views.
pub fn placeholder(base: usize, len: usize) {
    if !on() || base == 0 {
        return;
    }
    let end = base + len;
    let mut ranges = RANGES.write().unwrap_or_else(|e| e.into_inner());
    ranges.retain(|r| r.end <= base || r.base >= end);
    let words = len.div_ceil(K64).div_ceil(64);
    ranges.push(Range { base, end, shown: (0..words).map(|_| AtomicU64::new(0)).collect() });
}

impl Range {
    fn is_shown(&self, at: usize) -> bool {
        let block = (at - self.base) / K64;
        self.shown[block / 64].load(Relaxed) >> (block % 64) & 1 != 0
    }

    /// Where the run of blocks in the same state as `at`'s ends.
    fn run_end(&self, at: usize) -> usize {
        let state = self.is_shown(at);
        let same = if state { u64::MAX } else { 0 };
        let blocks = (self.end - self.base).div_ceil(K64);
        let mut block = (at - self.base) / K64 + 1;
        while block < blocks {
            if block.is_multiple_of(64) && self.shown[block / 64].load(Relaxed) == same {
                block += 64;
            } else if (self.shown[block / 64].load(Relaxed) >> (block % 64) & 1 != 0) == state {
                block += 1;
            } else {
                break;
            }
        }
        (self.base + block * K64).min(self.end)
    }
}

fn note(at: usize, len: usize, shown: bool) {
    let ranges = ranges();
    for block in (at..at + len).step_by(K64) {
        let Some(r) = ranges.iter().find(|r| r.base <= block && block < r.end) else { continue };
        let k = (block - r.base) / K64;
        let bit = 1u64 << (k % 64);
        if shown {
            r.shown[k / 64].fetch_or(bit, Relaxed);
        } else {
            r.shown[k / 64].fetch_and(!bit, Relaxed);
        }
    }
}

/// Whether `at` lies in a placeholder for page views, and if so whether it
/// shows a page and where that state ends.
fn state(at: usize) -> Option<(bool, usize)> {
    if !on() {
        return None;
    }
    let ranges = ranges();
    let r = ranges.iter().find(|r| r.base <= at && at < r.end)?;
    Some((r.is_shown(at), r.run_end(at)))
}

/// Whether a 64 KiB block of `[at, at + len)` lies in a placeholder for
/// page views and shows nothing: a read there faults.
pub(crate) fn hidden_within(at: usize, len: usize) -> bool {
    if !on() || len == 0 {
        return false;
    }
    let ranges = ranges();
    let end = at.saturating_add(len);
    (at & !(K64 - 1)..end)
        .step_by(K64)
        .any(|block| ranges.iter().find(|r| r.base <= block && block < r.end).is_some_and(|r| !r.is_shown(block)))
}

/// Whether `at` lies in a page view of ours.
pub fn is_shown(at: usize) -> bool {
    state(at).is_some_and(|s| s.0)
}

/// `VirtualQuery`, knowing the page views Wine does not.
///
/// # Safety
/// `out` must be writable.
#[allow(non_snake_case)]
pub unsafe fn Query(at: *const c_void, out: *mut MEMORY_BASIC_INFORMATION, size: usize) -> usize {
    let Some((shown, end)) = state(at as usize) else {
        // SAFETY: the caller's.
        return unsafe { VirtualQuery(at, out, size) };
    };
    let base = at as usize & !4095;
    let info = MEMORY_BASIC_INFORMATION {
        BaseAddress: base as *mut c_void,
        AllocationBase: base as *mut c_void,
        AllocationProtect: PAGE_READWRITE,
        RegionSize: end - base,
        State: if shown { MEM_COMMIT } else { MEM_RESERVE },
        Protect: if shown { PAGE_READWRITE } else { PAGE_NOACCESS },
        Type: if shown { MEM_MAPPED } else { MEM_PRIVATE },
        ..Default::default()
    };
    // SAFETY: the caller's.
    unsafe { out.write(info) };
    size_of::<MEMORY_BASIC_INFORMATION>()
}

/// `VirtualProtect` to read-only or read-write, for page views and other
/// memory alike.
///
/// # Safety
/// As `VirtualProtect`.
#[allow(non_snake_case)]
pub unsafe fn Protect(at: *const c_void, len: usize, protection: u32, old: *mut u32) -> i32 {
    let Some((mut shown, mut end)) = state(at as usize) else {
        // A range of title memory with holes can have memory of the process
        // itself in them (Wine's heaps lie among the title's low addresses):
        // write-protected, the next allocation faults under the guards' lock.
        if on() && protection == PAGE_READONLY {
            return 0;
        }
        // SAFETY: the caller's.
        return unsafe { VirtualProtect(at, len, protection, old) };
    };
    let to = at as usize + len;
    while shown && end < to {
        let Some(next) = state(end) else { return 0 };
        (shown, end) = next;
    }
    if !shown {
        return 0;
    }
    let access = if protection == PAGE_READONLY { PROT_READ } else { PROT_READ_WRITE };
    let done = native().map_or(-1, |n| n.call(MPROTECT, [at as i64, len as i64, access, 0, 0, 0]));
    i32::from(done == 0)
}

/// Copies within this process; false if a page of either side is not there
/// (or not readable, not writable). `None` without the host's calls.
fn copy(number: i64, ours: *const u8, title: *const u8, len: usize) -> Option<bool> {
    let n = native()?;
    let mut done = 0;
    while done < len {
        let local = [ours as usize + done, len - done];
        let remote = [title as usize + done, len - done];
        let k = n.call(number, [n.pid, local.as_ptr() as i64, 1, remote.as_ptr() as i64, 1, 0]);
        if k <= 0 {
            return Some(false);
        }
        done += k as usize;
    }
    Some(true)
}

/// `ReadProcessMemory` on our own process.
pub(crate) fn read(title: *const u8, to: *mut u8, len: usize) -> Option<bool> {
    copy(READV, to, title, len)
}

/// `WriteProcessMemory` on our own process, which opens no protection.
pub(crate) fn write(title: *mut u8, from: *const u8, len: usize) -> Option<bool> {
    copy(WRITEV, from, title, len)
}

/// How many of the 4 KiB pages `first..=last` can be read; with `all`, stops
/// at the first that cannot.
pub(crate) fn readable(first: usize, last: usize, all: bool) -> Option<usize> {
    let n = native()?;
    let mut there = 0;
    let mut page = first;
    let mut bytes = [0u8; 256];
    while page <= last {
        let k = (last - page + 1).min(256);
        let local = [bytes.as_mut_ptr() as usize, k];
        let mut remote = [[0usize; 2]; 256];
        for (i, r) in remote[..k].iter_mut().enumerate() {
            *r = [(page + i) << 12, 1];
        }
        let got = n.call(READV, [n.pid, local.as_ptr() as i64, 1, remote.as_ptr() as i64, k as i64, 0]);
        let got = got.max(0) as usize;
        there += got;
        page += got;
        if got < k {
            if all {
                break;
            }
            page += 1;
        }
    }
    Some(there)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_of_shown_blocks_end_where_the_state_changes() {
        let r = Range { base: 0x10000, end: 0x10000 + 200 * K64, shown: (0..4).map(|_| AtomicU64::new(0)).collect() };
        assert!(!r.is_shown(r.base));
        assert_eq!(r.run_end(r.base), r.end);
        for k in 3..150 {
            r.shown[k / 64].fetch_or(1 << (k % 64), Relaxed);
        }
        assert_eq!(r.run_end(r.base + 5), r.base + 3 * K64);
        assert!(r.is_shown(r.base + 3 * K64 + 7));
        assert_eq!(r.run_end(r.base + 3 * K64), r.base + 150 * K64);
        assert_eq!(r.run_end(r.base + 150 * K64), r.end);
    }
}
