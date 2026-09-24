//! Running the task's application inside its dev container.
//!
//! The full-stack Rust profile knows which workspace member is the API
//! binary and which is the trunk-built frontend. This module turns that
//! knowledge into three agent tools: `app_start` builds both, starts the API
//! on a per-task port and waits for its health endpoint; `app_stop` kills it;
//! `app_logs` tails its log. Ports come from [`PortAllocator`], running
//! instances are tracked in [`RunningApps`] so that a second `app_start`
//! returns the instance already running.

pub mod instance;
pub mod ports;

pub use instance::{AppInstance, Limits, RunningApps, DEFAULT_MAX_WALL_CLOCK_SECS};
pub use ports::{PortAllocator, PortError, PortLease, DEFAULT_PORT_RANGE, LEASE_DIR_NAME};
