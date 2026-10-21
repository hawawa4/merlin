//! The built-in effects (plan §4.3): EQ, compressor, delay, reverb, and the master limiter.
//! Pure stereo `f32` DSP: everything is allocated when an effect is created, nothing in the
//! processing calls.

mod biquad;
mod chain;
mod comp;
mod delay;
mod eq;
mod limiter;
mod reverb;
mod util;

pub use chain::FxChain;
pub use comp::Compressor;
pub use delay::Delay;
pub use eq::Eq;
pub use limiter::Limiter;
pub use reverb::Reverb;
pub use util::{db_to_lin, lin_to_db, rms};
