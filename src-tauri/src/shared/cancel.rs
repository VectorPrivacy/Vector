//! Cooperative cancellation for CPU work running on one blocking thread: the caller installs
//! a flag for the duration of the work, and long loops poll `check()` between steps.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

thread_local! {
    static FLAG: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

pub const CANCELLED: &str = "compression cancelled";

/// Run `f` with `flag` as this thread's cancel flag.
pub fn scoped<R>(flag: Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FLAG.with(|c| *c.borrow_mut() = None);
        }
    }
    FLAG.with(|c| *c.borrow_mut() = Some(flag));
    let _reset = Reset;
    f()
}

/// Err once the installed flag is set; always Ok with none installed.
pub fn check() -> Result<(), String> {
    let set = FLAG.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)));
    if set { Err(CANCELLED.into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_follows_the_installed_flag_and_clears_after() {
        let flag = Arc::new(AtomicBool::new(false));
        scoped(flag.clone(), || {
            assert!(check().is_ok());
            flag.store(true, Ordering::Relaxed);
            assert!(check().is_err());
        });
        assert!(check().is_ok());
    }
}
