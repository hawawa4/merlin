//! Denormal numbers: what to do about them on an audio thread.
//!
//! A decaying filter or resonator tail ends in denormals, and arithmetic on those can run
//! 10 to 100 times slower than on normal floats. Hosts set the CPU's flush-to-zero (FTZ) and
//! denormals-are-zero (DAZ) flags on every thread that runs plugin code, so a plugin that does
//! not guard against them itself does not blow up the CPU load as its notes die away.
//!
//! The flags belong to a thread, and a new thread starts with the process' defaults: whoever
//! runs plugin code calls [`flush_to_zero`] on that thread (the app's audio callback, and the
//! audio thread of each plugin host process).

/// Sets FTZ and DAZ on the calling thread.
pub fn flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads and sets this thread's MXCSR; no memory is touched.
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= FTZ_DAZ;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack));
    }
}

/// MXCSR bit 15 (flush to zero) and bit 6 (denormals are zero).
#[cfg(target_arch = "x86_64")]
const FTZ_DAZ: u32 = 0x8040;

/// Are FTZ and DAZ both set on the calling thread? (Always true where there is nothing to set.)
pub fn is_flushing_to_zero() -> bool {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads this thread's MXCSR; no memory is touched.
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr & FTZ_DAZ == FTZ_DAZ
    }
    #[cfg(not(target_arch = "x86_64"))]
    true
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn a_new_thread_does_not_flush_until_asked() {
        std::thread::spawn(|| {
            assert!(!is_flushing_to_zero());
            flush_to_zero();
            assert!(is_flushing_to_zero());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn flushing_makes_denormal_arithmetic_vanish() {
        std::thread::spawn(|| {
            // Compiler-opaque so the products are really computed at run time.
            let tiny = std::hint::black_box(f32::MIN_POSITIVE);
            let half = std::hint::black_box(0.5f32);
            assert!(tiny * half > 0.0, "a denormal result without FTZ");
            flush_to_zero();
            assert_eq!(tiny * half, 0.0);
        })
        .join()
        .unwrap();
    }
}
