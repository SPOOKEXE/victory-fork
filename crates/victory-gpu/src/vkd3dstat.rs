//! vkd3d-proton's memory allocator, from inside, for the overlay's vkd3d
//! view (F2) and each snapshot (F1): the counters of `vkd3d_stats`, read
//! from the host device. Without the patched vkd3d-proton
//! (`native/build-vkd3d.sh`) there is no export and nothing to show.

use std::sync::Mutex;
pub(crate) use vkd3d_stats::Row;
use vkd3d_stats::{Export, at, lay_out};

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
    let mut rows = lay_out(&now, &before, seconds);
    // What of ours asks for the memory: our buffers by the code that made
    // them, the most bytes first.
    for (file, line, kind, count, bytes) in crate::host::makers().into_iter().take(12) {
        let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
        rows.push(("our buffers made since the start (MB, count)", format!("{file}:{line} {kind}"), bytes as f64 / f64::from(1u32 << 20), count as f64, 4096.0));
    }
    rows
}
