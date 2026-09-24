//! Cross-agent coordination leases.
//!
//! Many agents run at once and their effects must not collide: two rollouts
//! to one environment, two pushes to one branch, two sandbox deploys for one
//! pull request, or two tasks editing the same path set. A [`LeaseName`]
//! identifies each such resource and a [`LeaseStore`] hands out named,
//! TTL-bearing leases on them.

mod jsonl;
mod name;
mod store;

pub use jsonl::{default_lease_path, lease_path_from, JsonlLeaseStore, LEASE_PATH_ENV};
pub use name::{LeaseKind, LeaseName};
pub use store::{InMemoryLeaseStore, Lease, LeaseError, LeaseStore, LeaseToken};
