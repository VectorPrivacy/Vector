//! glibc keeps freed blocks in each thread's arena; after a burst of large transient buffers
//! (a decode, a bake) that is megabytes the process holds and never uses again. Requests
//! coalesce onto one detached thread, so a run of decodes costs one trim.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
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
                            unsafe { libc::malloc_trim(0) };
                        }
                    })
                    .expect("trim thread")
                    .thread()
                    .clone()
            })
            .unpark();
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use imp::soon;

/// Other allocators return memory on their own.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn soon() {}

/// Asks for a trim once the scope it guards ends, on every return path.
pub struct OnDrop;

impl Drop for OnDrop {
    fn drop(&mut self) {
        soon();
    }
}
