//! Thread-locals that cost a load, for the hot paths of the title's threads.
//!
//! On `windows-gnu` the standard `thread_local!` has no native TLS: every
//! access asks the system for the value (`TlsGetValue`) and first runs
//! `guard::enable`, which adds to and takes from one counter all threads
//! share (a locked add each way, a full fence) and calls `FlsSetValue`.
//! With a score of title threads recording at once that counter's cache
//! line went from processor to processor: 18 % of our code's time on those
//! threads in the café (run J), most of it in draws' binding.
//!
//! [`fast_local!`](crate::fast_local) takes the same declarations as
//! `thread_local!` and gives the same methods (`with`, `get`, `set`,
//! `take`, `replace`, `with_borrow`, `with_borrow_mut`). On Windows a value
//! lives in a TLS slot of its own (`TlsAlloc`, once), read straight from the
//! thread's TEB: one load from `gs`, no call, no shared write. Elsewhere the
//! macro is `thread_local!` itself.
//!
//! What differs: a thread's value is never dropped (its memory, from the
//! system allocator, is left when the thread ends), so a value that counts
//! or gathers for others hands its last few over only as it fills, as
//! before; nothing here needs its drop to run.

/// `thread_local!`, without the per-access cost on Windows (see the module).
#[macro_export]
macro_rules! fast_local {
    () => {};
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }; $($rest:tt)*) => {
        $crate::fast_local!(@one $(#[$attr])* $vis $name, $t, $init);
        $crate::fast_local!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = const { $init:expr }) => {
        $crate::fast_local!(@one $(#[$attr])* $vis $name, $t, $init);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr; $($rest:tt)*) => {
        $crate::fast_local!(@one $(#[$attr])* $vis $name, $t, $init);
        $crate::fast_local!($($rest)*);
    };
    ($(#[$attr:meta])* $vis:vis static $name:ident: $t:ty = $init:expr) => {
        $crate::fast_local!(@one $(#[$attr])* $vis $name, $t, $init);
    };
    (@one $(#[$attr:meta])* $vis:vis $name:ident, $t:ty, $init:expr) => {
        #[cfg(windows)]
        $(#[$attr])*
        $vis static $name: $crate::fastlocal::Local<$t> = {
            fn init() -> $t {
                $init
            }
            $crate::fastlocal::Local::new(init)
        };
        #[cfg(not(windows))]
        ::std::thread_local! {
            $(#[$attr])*
            $vis static $name: $t = $init;
        }
    };
}

#[cfg(windows)]
pub use imp::Local;

#[cfg(windows)]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A thread-local value: its TLS slot (`u32::MAX` until the first use)
    /// and how a thread's value starts.
    pub struct Local<T: 'static> {
        slot: AtomicU32,
        init: fn() -> T,
    }

    // SAFETY: each thread only ever sees its own value.
    unsafe impl<T> Sync for Local<T> {}

    /// The TEB's 64 TLS slots, and the pointer to the 1,024 after them.
    const SLOTS: usize = 0x1480;
    const EXPANSION: usize = 0x1780;

    /// This thread's value in `slot`, read from the TEB; null if none yet.
    #[inline(always)]
    fn read(slot: u32) -> *mut u8 {
        let value: *mut u8;
        // SAFETY: `gs` is the thread's TEB on x86-64 Windows (and in Wine,
        // which lays it out the same); the slot was given by `TlsAlloc`.
        unsafe {
            if slot < 64 {
                std::arch::asm!("mov {}, gs:[{} * 8 + {s}]", out(reg) value, in(reg) slot as usize, s = const SLOTS,
                    options(nostack, readonly, preserves_flags));
            } else {
                let more: *const *mut u8;
                std::arch::asm!("mov {}, gs:[{e}]", out(reg) more, e = const EXPANSION, options(nostack, readonly, preserves_flags));
                if more.is_null() {
                    return std::ptr::null_mut();
                }
                value = *more.add(slot as usize - 64);
            }
        }
        value
    }

    impl<T: 'static> Local<T> {
        #[doc(hidden)]
        pub const fn new(init: fn() -> T) -> Self {
            Local { slot: AtomicU32::new(u32::MAX), init }
        }

        /// The slot, allocated by the first thread to ask.
        #[cold]
        fn allocate(&self) -> u32 {
            use windows_sys::Win32::System::Threading::{TLS_OUT_OF_INDEXES, TlsAlloc, TlsFree};
            // SAFETY: plain calls.
            let mine = unsafe { TlsAlloc() };
            if mine == TLS_OUT_OF_INDEXES {
                eprintln!("VIctory: no TLS slot left for a thread-local");
                std::process::abort();
            }
            match self.slot.compare_exchange(u32::MAX, mine, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => mine,
                Err(theirs) => {
                    // SAFETY: ours, never used.
                    unsafe { TlsFree(mine) };
                    theirs
                }
            }
        }

        /// This thread's value, made the first time (from the system
        /// allocator: the counting allocator uses these too).
        #[cold]
        fn make(&self, slot: u32) -> *mut T {
            let layout = Layout::new::<T>();
            // SAFETY: a fresh allocation of `T`'s layout, written before use;
            // a zero-sized `T` takes a dangling, aligned pointer.
            let at = if layout.size() == 0 {
                std::ptr::NonNull::<T>::dangling().as_ptr()
            } else {
                let at = unsafe { System.alloc(layout) }.cast::<T>();
                if at.is_null() {
                    std::alloc::handle_alloc_error(layout);
                }
                at
            };
            // SAFETY: as above.
            unsafe { at.write((self.init)()) };
            // SAFETY: our slot.
            if unsafe { windows_sys::Win32::System::Threading::TlsSetValue(slot, at.cast()) } == 0 {
                eprintln!("VIctory: a thread-local's value could not be set");
                std::process::abort();
            }
            at
        }

        #[inline(always)]
        fn here(&'static self) -> &'static T {
            let mut slot = self.slot.load(Ordering::Acquire);
            if slot == u32::MAX {
                slot = self.allocate();
            }
            let mut at = read(slot).cast::<T>();
            if at.is_null() {
                at = self.make(slot);
            }
            // SAFETY: this thread's value, never freed while it runs; only
            // this thread reaches it.
            unsafe { &*at }
        }

        /// As `LocalKey::with`.
        #[inline]
        pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
            f(self.here())
        }

        /// As `LocalKey::try_with` (it never fails here).
        #[inline]
        pub fn try_with<R>(&'static self, f: impl FnOnce(&T) -> R) -> Result<R, std::convert::Infallible> {
            Ok(f(self.here()))
        }
    }

    impl<T: 'static> Local<Cell<T>> {
        #[inline]
        pub fn set(&'static self, value: T) {
            self.here().set(value);
        }

        #[inline]
        pub fn replace(&'static self, value: T) -> T {
            self.here().replace(value)
        }

        #[inline]
        pub fn get(&'static self) -> T
        where
            T: Copy,
        {
            self.here().get()
        }

        #[inline]
        pub fn take(&'static self) -> T
        where
            T: Default,
        {
            self.here().take()
        }
    }

    impl<T: 'static> Local<RefCell<T>> {
        #[inline]
        pub fn with_borrow<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
            f(&self.here().borrow())
        }

        #[inline]
        pub fn with_borrow_mut<R>(&'static self, f: impl FnOnce(&mut T) -> R) -> R {
            f(&mut self.here().borrow_mut())
        }

        #[inline]
        pub fn set(&'static self, value: T) {
            *self.here().borrow_mut() = value;
        }

        #[inline]
        pub fn replace(&'static self, value: T) -> T {
            self.here().replace(value)
        }

        #[inline]
        pub fn take(&'static self) -> T
        where
            T: Default,
        {
            self.here().take()
        }
    }

    #[cfg(test)]
    mod tests {
        use std::cell::{Cell, RefCell};

        crate::fast_local! {
            static COUNT: Cell<u32> = const { Cell::new(7) };
            static LIST: RefCell<Vec<u32>> = RefCell::new(vec![1]);
        }

        #[test]
        fn each_thread_starts_from_the_initial_value_and_keeps_its_own() {
            COUNT.set(COUNT.get() + 1);
            LIST.with_borrow_mut(|l| l.push(2));
            assert_eq!(COUNT.get(), 8);
            assert_eq!(LIST.with_borrow(Vec::clone), vec![1, 2]);
            std::thread::spawn(|| {
                assert_eq!(COUNT.get(), 7);
                assert_eq!(LIST.take(), vec![1]);
                COUNT.set(100);
            })
            .join()
            .unwrap();
            assert_eq!(COUNT.replace(0), 8);
            assert_eq!(LIST.with_borrow(Vec::len), 2);
        }

        /// More slots than the TEB's 64 direct ones: the rest are read
        /// through its expansion pointer.
        #[test]
        fn slots_past_the_first_64_read_back() {
            use windows_sys::Win32::System::Threading::{TlsAlloc, TlsSetValue};
            let mut slots = Vec::new();
            for i in 0..80usize {
                // SAFETY: fresh slots of our own, left allocated.
                let slot = unsafe { TlsAlloc() };
                unsafe { TlsSetValue(slot, (i + 1) as *const core::ffi::c_void) };
                slots.push((slot, i + 1));
            }
            for (slot, value) in slots {
                assert_eq!(super::read(slot) as usize, value, "slot {slot}");
            }
        }
    }
}
