//! The title's calls into the runtime, timed for the frame graph
//! (`victory_gpu::trace`): each import we bind to one of our own functions
//! (`hle: <name>`), or to one of the host's waits and sleeps (`host:
//! <name>`), goes through a small stub of machine code that notes the time
//! and the return address on the way in, and on the way out records the call
//! as a span on the calling thread's track. What the time line showed as
//! "the title's own work" is then split into what the title did and what it
//! waited for in us or in the host.
//!
//! `VICTORY_TIME_IMPORTS=0` binds the functions themselves, as before.
//!
//! How a stub works (x86-64, Windows calling convention): the import's stub
//! puts the address of its [`Target`] in `rax` and jumps to [`common`]. That saves the four
//! argument registers in the caller's home space and the four float ones on
//! its own stack, calls [`enter`] with that, the return address and
//! the stack pointer, restores the arguments, puts the address of the exit
//! code where the return address was and jumps to the function. The function
//! returns into the exit code, which calls [`exit`] (the span, and the real
//! return address back) with the return values saved around it, and jumps
//! there. The stack the function sees is the one the title gave it.
//!
//! What a stub cannot do: be unwound through. An exception that leaves the
//! function for the title's handlers would find the exit code's address as
//! a return address with no unwind information. Our own functions return or
//! abort, and the host's waits and sleeps return; nothing else is wrapped.
//! A call that never returns (a thread's exit) leaves its note behind, which
//! the next return on the thread clears ([`exit`] drops notes deeper than
//! the frame returning).

/// The host functions timed: the waits and sleeps the title may stand in.
const HOST_TIMED: &[&str] = &[
    "WaitForSingleObject",
    "WaitForSingleObjectEx",
    "WaitForMultipleObjects",
    "WaitForMultipleObjectsEx",
    "SignalObjectAndWait",
    "Sleep",
    "SleepEx",
    "SwitchToThread",
    "SleepConditionVariableSRW",
    "SleepConditionVariableCS",
    "WaitOnAddress",
    "GetQueuedCompletionStatus",
    "GetQueuedCompletionStatusEx",
];

/// Whether imports are timed (`VICTORY_TIME_IMPORTS`, on unless `0`).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| cfg!(windows) && std::env::var_os("VICTORY_TIME_IMPORTS").is_none_or(|v| v != "0"))
}

/// Whether host function `name` is timed.
pub(crate) fn host_timed(name: &str) -> bool {
    HOST_TIMED.contains(&name)
}

/// A stubbed function: its address and span name. Leaked; its address is the
/// stub's number.
struct Target {
    address: usize,
    name: &'static str,
}

/// The address the title binds for `target`, timed as `name`: a stub of its
/// own, or `target` itself if no stub can be made.
pub(crate) fn wrap(target: usize, name: String) -> usize {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        let id: &'static Target = Box::leak(Box::new(Target {
            address: target,
            name: Box::leak(name.into_boxed_str()),
        }));
        match stubs::stub(id as *const Target as u64) {
            Some(stub) => stub,
            None => target,
        }
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        let _ = name;
        target
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod stubs {
    use std::cell::RefCell;
    use std::sync::{Mutex, OnceLock};

    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};

    /// Executable memory stubs are written into: (next free byte, end).
    static CODE: Mutex<(usize, usize)> = Mutex::new((0, 0));
    const CHUNK: usize = 1 << 20;

    /// Copies `code` into executable memory; its address.
    fn place(code: &[u8]) -> Option<usize> {
        let mut at = CODE.lock().unwrap_or_else(|e| e.into_inner());
        if at.0 + code.len() > at.1 {
            // SAFETY: a fresh allocation of our own.
            let base = unsafe { VirtualAlloc(std::ptr::null(), CHUNK, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) };
            if base.is_null() {
                return None;
            }
            *at = (base as usize, base as usize + CHUNK);
        }
        let here = at.0;
        // SAFETY: inside the allocation, which nothing else writes.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), here as *mut u8, code.len()) };
        at.0 = (here + code.len() + 15) & !15;
        Some(here)
    }

    /// The code all stubs share: (the entry, the exit code's address).
    fn common() -> Option<(usize, usize)> {
        static COMMON: OnceLock<Option<(usize, usize)>> = OnceLock::new();
        *COMMON.get_or_init(|| {
            let exit_code = {
                let mut c: Vec<u8> = Vec::new();
                c.extend([0x48, 0x83, 0xEC, 0x40]); // sub rsp, 0x40
                c.extend([0x48, 0x89, 0x44, 0x24, 0x20]); // mov [rsp+0x20], rax
                c.extend([0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x28]); // movdqu [rsp+0x28], xmm0
                c.extend([0x48, 0x8D, 0x4C, 0x24, 0x38]); // lea rcx, [rsp+0x38]: the stack at entry
                c.extend([0x49, 0xBB]); // mov r11, exit
                c.extend((exit as *const () as u64).to_le_bytes());
                c.extend([0x41, 0xFF, 0xD3]); // call r11
                c.extend([0x49, 0x89, 0xC3]); // mov r11, rax: the return address
                c.extend([0x48, 0x8B, 0x44, 0x24, 0x20]); // mov rax, [rsp+0x20]
                c.extend([0xF3, 0x0F, 0x6F, 0x44, 0x24, 0x28]); // movdqu xmm0, [rsp+0x28]
                c.extend([0x48, 0x83, 0xC4, 0x40]); // add rsp, 0x40
                c.extend([0x41, 0xFF, 0xE3]); // jmp r11
                place(&c)?
            };
            let mut c: Vec<u8> = Vec::new();
            c.extend([0x48, 0x89, 0x4C, 0x24, 0x08]); // mov [rsp+8], rcx
            c.extend([0x48, 0x89, 0x54, 0x24, 0x10]); // mov [rsp+0x10], rdx
            c.extend([0x4C, 0x89, 0x44, 0x24, 0x18]); // mov [rsp+0x18], r8
            c.extend([0x4C, 0x89, 0x4C, 0x24, 0x20]); // mov [rsp+0x20], r9
            c.extend([0x48, 0x83, 0xEC, 0x68]); // sub rsp, 0x68
            c.extend([0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x20]); // movdqu [rsp+0x20], xmm0
            c.extend([0xF3, 0x0F, 0x7F, 0x4C, 0x24, 0x30]); // movdqu [rsp+0x30], xmm1
            c.extend([0xF3, 0x0F, 0x7F, 0x54, 0x24, 0x40]); // movdqu [rsp+0x40], xmm2
            c.extend([0xF3, 0x0F, 0x7F, 0x5C, 0x24, 0x50]); // movdqu [rsp+0x50], xmm3
            c.extend([0x48, 0x89, 0xC1]); // mov rcx, rax: the stub's number
            c.extend([0x48, 0x8B, 0x54, 0x24, 0x68]); // mov rdx, [rsp+0x68]: the return address
            c.extend([0x4C, 0x8D, 0x44, 0x24, 0x68]); // lea r8, [rsp+0x68]: the stack at entry
            c.extend([0x49, 0xBB]); // mov r11, enter
            c.extend((enter as *const () as u64).to_le_bytes());
            c.extend([0x41, 0xFF, 0xD3]); // call r11
            c.extend([0xF3, 0x0F, 0x6F, 0x44, 0x24, 0x20]); // movdqu xmm0, [rsp+0x20]
            c.extend([0xF3, 0x0F, 0x6F, 0x4C, 0x24, 0x30]); // movdqu xmm1, [rsp+0x30]
            c.extend([0xF3, 0x0F, 0x6F, 0x54, 0x24, 0x40]); // movdqu xmm2, [rsp+0x40]
            c.extend([0xF3, 0x0F, 0x6F, 0x5C, 0x24, 0x50]); // movdqu xmm3, [rsp+0x50]
            c.extend([0x48, 0x83, 0xC4, 0x68]); // add rsp, 0x68
            c.extend([0x48, 0x8B, 0x4C, 0x24, 0x08]); // mov rcx, [rsp+8]
            c.extend([0x48, 0x8B, 0x54, 0x24, 0x10]); // mov rdx, [rsp+0x10]
            c.extend([0x4C, 0x8B, 0x44, 0x24, 0x18]); // mov r8, [rsp+0x18]
            c.extend([0x4C, 0x8B, 0x4C, 0x24, 0x20]); // mov r9, [rsp+0x20]
            c.extend([0x49, 0xBB]); // mov r11, the exit code
            c.extend((exit_code as u64).to_le_bytes());
            c.extend([0x4C, 0x89, 0x1C, 0x24]); // mov [rsp], r11: it is returned into
            c.extend([0xFF, 0xE0]); // jmp rax: the function
            Some((place(&c)?, exit_code))
        })
    }

    /// The stub of the import whose [`Target`](super::Target) is at `id`.
    pub(super) fn stub(id: u64) -> Option<usize> {
        let (entry, _) = common()?;
        let mut c: Vec<u8> = Vec::new();
        c.extend([0x48, 0xB8]); // mov rax, id
        c.extend(id.to_le_bytes());
        c.extend([0x49, 0xBB]); // mov r11, the common entry
        c.extend((entry as u64).to_le_bytes());
        c.extend([0x41, 0xFF, 0xE3]); // jmp r11
        place(&c)
    }

    std::thread_local! {
        /// Calls in progress on this thread: return address, stub number, start, stack at entry.
        static CALLS: RefCell<Vec<(u64, u64, u64, u64)>> = const { RefCell::new(Vec::new()) };
    }

    /// On the way in: notes the call; the function's address. The thread's
    /// last error is kept as it was, as in [`exit`].
    extern "win64" fn enter(id: u64, ret: u64, rsp: u64) -> u64 {
        // SAFETY: plain calls.
        let error = unsafe { GetLastError() };
        // SAFETY: `id` is a leaked `Target` (see `wrap`).
        let target = unsafe { &*(id as *const super::Target) }.address;
        let at = if victory_gpu::trace::timing() { victory_gpu::clock::now_ns() } else { 0 };
        CALLS.with_borrow_mut(|calls| calls.push((ret, id, at, rsp)));
        // SAFETY: as above.
        unsafe { SetLastError(error) };
        target as u64
    }

    /// On the way out of the call whose stack at entry was `rsp`: the span,
    /// and the return address. Notes of calls deeper than it that never came
    /// back (a thread left from inside one) are dropped.
    ///
    /// The function's last error is kept for the title: what we do here (a
    /// thread-local, a lock) may reset it, and a title that sees no
    /// `ERROR_IO_PENDING` after a `ReadFile` of its own retries for ever.
    extern "win64" fn exit(rsp: u64) -> u64 {
        // SAFETY: plain calls.
        let error = unsafe { GetLastError() };
        let note = CALLS.with_borrow_mut(|calls| {
            while calls.last().is_some_and(|c| c.3 < rsp) {
                calls.pop();
            }
            calls.pop()
        });
        let Some((ret, id, at, entry)) = note else {
            // No note: the return address is lost. Nothing can go on.
            eprintln!("VIctory: an import's timing stub found no note of its call (stack {rsp:#x})");
            std::process::abort();
        };
        debug_assert_eq!(entry, rsp);
        if at != 0 {
            // SAFETY: as in `enter`.
            let name = unsafe { &*(id as *const super::Target) }.name;
            victory_gpu::trace::timed(name, at);
        }
        // SAFETY: as above.
        unsafe { SetLastError(error) };
        ret
    }
}
