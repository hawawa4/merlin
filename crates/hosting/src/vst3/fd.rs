//! Watches a plugin's run-loop fds on a helper thread and wakes the app when one is readable.
//! After waking it waits for an ack, so a still-readable fd cannot make it spin.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::{HostEvent, Waker};

pub struct FdPoller {
    list: Arc<Mutex<Vec<libc::pollfd>>>,
    wake_w: RawFd,
    ack: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
}

impl FdPoller {
    pub fn spawn(waker: Arc<dyn Waker>) -> Self {
        let mut pipe = [0; 2];
        // SAFETY: plain pipe creation.
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        let (wake_r, wake_w) = (pipe[0], pipe[1]);
        let list: Arc<Mutex<Vec<libc::pollfd>>> = Arc::default();
        let (ack, ack_rx) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_list, thread_stop) = (list.clone(), stop.clone());
        std::thread::Builder::new()
            .name("vst3-fd-poller".into())
            .spawn(move || {
                loop {
                    let mut fds = vec![libc::pollfd {
                        fd: wake_r,
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    fds.extend(thread_list.lock().unwrap().iter().copied());
                    // SAFETY: fds is a valid array of pollfd.
                    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
                    if n < 0 {
                        continue;
                    }
                    if fds[0].revents != 0 {
                        let mut buf = [0u8; 64];
                        // SAFETY: draining our own non-blocking pipe.
                        while unsafe { libc::read(wake_r, buf.as_mut_ptr().cast(), 64) } > 0 {}
                        if thread_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                    waker.wake(HostEvent::FdReady);
                    if ack_rx.recv().is_err() {
                        break;
                    }
                }
                // SAFETY: the read end is ours alone and nothing polls it again.
                unsafe { libc::close(wake_r) };
            })
            .expect("spawn fd poller");
        Self {
            list,
            wake_w,
            ack,
            stop,
        }
    }

    pub fn set(&self, fds: &[RawFd]) {
        *self.list.lock().unwrap() = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: one byte into our own pipe to interrupt poll().
        unsafe { libc::write(self.wake_w, [1u8].as_ptr().cast(), 1) };
    }

    /// Main thread, after `FdReady`: which of the watched fds are readable now.
    pub fn ready(&self, fds: &[RawFd]) -> Vec<RawFd> {
        let mut pfds: Vec<libc::pollfd> = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: valid pollfd array, zero timeout.
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 0) };
        pfds.iter()
            .filter(|p| p.revents != 0)
            .map(|p| p.fd)
            .collect()
    }

    pub fn ack(&self) {
        let _ = self.ack.send(());
    }
}

impl Drop for FdPoller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // SAFETY: wake the poller so it sees `stop`, then close our write end.
        unsafe {
            libc::write(self.wake_w, [1u8].as_ptr().cast(), 1);
            libc::close(self.wake_w);
        }
    }
}
