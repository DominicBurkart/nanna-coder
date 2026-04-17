//! Terminal UI utilities for the harness CLI.
//!
//! Currently provides:
//! - [`spinner`]: a moon-phase animated progress indicator that renders to
//!   stderr while async work is in progress.
//! - [`log_writer`]: a [`tracing_subscriber`] writer that coordinates with the
//!   active spinner so log emissions do not interleave mid-frame.

pub mod log_writer;
pub mod spinner;

pub use log_writer::SpinnerAwareMakeWriter;
pub use spinner::{next_glyph, AnimationPolicy, MoonSpinner, SpinnerGuard, MOON_PHASES};
