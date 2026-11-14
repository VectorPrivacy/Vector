//! glibc keeps freed blocks in each thread's arena, and macOS's malloc keeps emptied regions
//! dirty until the system runs short; after a burst of large transient buffers (a decode, a bake,
//! a sync) that is megabytes the process holds and never uses again. Requests coalesce onto one
//! detached thread, so a run of decodes costs one trim.

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::thread::Thread;
    use std::time::Duration;

    static PENDING: AtomicBool = AtomicBool::new(false);
    static WORKER: OnceLock<Thread> = OnceLock::new();

    pub fn soon() {
        PENDING.store(true, Ordering::Release);
        WORKER
            .get_or_init(|| {
                // spawn-detached: process-lifetime allocator housekeeping, no account state.
                std::thread::Builder::new()
                    .name("malloc-trim".into())
                    .stack_size(64 * 1024)
                    .spawn(|| loop {
                        std::thread::park();
                        std::thread::sleep(Duration::from_millis(1500));
                        if PENDING.swap(false, Ordering::AcqRel) {
                            #[cfg(target_os = "linux")]
                            unsafe { libc::malloc_trim(0) };
                            // Every zone, as much as it can give back.
                            #[cfg(target_os = "macos")]
                            unsafe { super::malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
                        }
                    })
                    .expect("trim thread")
                    .thread()
                    .clone()
            })
            .unpark();
    }
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
pub use imp::soon;

/// Other allocators return memory on their own.
#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
pub fn soon() {}

/// Asks for a trim once the scope it guards ends, on every return path.
pub struct OnDrop;

impl Drop for OnDrop {
    fn drop(&mut self) {
        soon();
    }
}
