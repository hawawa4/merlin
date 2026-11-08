//! A short time slice for an audio thread that cannot be realtime.
//!
//! Without a realtime limit (no `pipewire` group, no rtkit) an audio thread competes with
//! everything else as an ordinary thread. On a busy machine a thread that is woken then waits for
//! the CPU until something else gives it up, and an audio thread that is woken twice per block
//! (the app's, and the plugin's in its own process) pays that wait twice.
//!
//! Since Linux 6.12 the scheduler (EEVDF) lets any thread ask for a shorter slice with
//! `sched_setattr`, no privilege needed: a short slice means an early virtual deadline, so the
//! thread preempts what is running when it wakes instead of queueing behind it. Older kernels
//! accept the call and ignore it.

use std::time::Duration;

/// `sched_setattr`'s argument, the first (48 byte) version.
#[repr(C)]
struct SchedAttr {
    size: u32,
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
}

/// The shortest slice the kernel takes (it clamps to 0.1 ms .. 100 ms).
pub const SHORTEST: Duration = Duration::from_micros(100);

/// Asks for `slice` as the calling thread's time slice, leaving its policy and nice level alone.
/// Returns whether the kernel accepted the request (which says nothing about older kernels
/// that do not act on it).
pub fn set_slice(slice: Duration) -> bool {
    // SAFETY: reading this thread's own nice level; getpriority(PRIO_PROCESS, 0) is the calling
    // thread on Linux, and -1 can be a legitimate answer, which is why errno is cleared first.
    let nice = unsafe {
        *libc::__errno_location() = 0;
        libc::getpriority(libc::PRIO_PROCESS, 0)
    };
    let attr = SchedAttr {
        size: std::mem::size_of::<SchedAttr>() as u32,
        policy: libc::SCHED_OTHER as u32,
        flags: 0,
        nice,
        priority: 0,
        runtime: slice.as_nanos() as u64,
        deadline: 0,
        period: 0,
    };
    // SAFETY: sched_setattr on the calling thread with a fully initialised attr of its stated size.
    unsafe { libc::syscall(libc::SYS_sched_setattr, 0, &attr, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slice the scheduler holds for thread `tid` of this process, in nanoseconds (`None`
    /// on kernels that do not print it).
    fn slice_of(tid: i64) -> Option<u64> {
        let sched = std::fs::read_to_string(format!("/proc/self/task/{tid}/sched")).ok()?;
        let line = sched
            .lines()
            .find(|l| l.trim_start().starts_with("se.slice"))?;
        line.split(':')
            .nth(1)?
            .trim()
            .parse::<f64>()
            .ok()
            .map(|ns| ns as u64)
    }

    fn tid() -> i64 {
        // SAFETY: gettid takes no arguments.
        unsafe { libc::syscall(libc::SYS_gettid) }
    }

    #[test]
    fn a_thread_gets_the_slice_it_asked_for_and_threads_it_starts_inherit_it() {
        std::thread::spawn(|| {
            let before = slice_of(tid());
            assert!(
                set_slice(SHORTEST),
                "the kernel refused sched_setattr: {}",
                std::io::Error::last_os_error()
            );
            let asked = SHORTEST.as_nanos() as u64;
            let child = std::thread::spawn(|| slice_of(tid())).join().unwrap();
            if let (Some(before), Some(after)) = (before, slice_of(tid())) {
                // A kernel that acts on it holds what we asked for; one that ignores it, what it had.
                assert!(after == asked || after == before, "{before} -> {after}");
                if after == asked {
                    // The helper threads a plugin starts from here play with the same slice.
                    assert_eq!(child, Some(asked));
                }
            }
        })
        .join()
        .unwrap();
    }
}
