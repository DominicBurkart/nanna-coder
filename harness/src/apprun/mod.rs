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
pub mod runner;
pub mod tools;

pub use instance::{AppInstance, Limits, RunningApps, DEFAULT_MAX_WALL_CLOCK_SECS};
pub use ports::{PortAllocator, PortError, PortLease, DEFAULT_PORT_RANGE, LEASE_DIR_NAME};
pub use runner::{
    api_build_argv, app_env, app_log_path, exec_args, executable_from_build_output,
    health_probe_argv, parse_pid, shell_quote, start_script, stop_app, tail_log, with_timeout,
    AppContext, AppError, AppSpec, StoppedApp, BIND_ADDR_VAR, DEFAULT_POLL_INTERVAL,
    DEFAULT_RUST_LOG, FRONTEND_DIST_VAR, LOG_DIR, LOG_TAIL_ON_FAILURE, RUST_LOG_VAR,
};
pub use tools::{register_app_tools, AppStartTool, APP_START_TOOL};
