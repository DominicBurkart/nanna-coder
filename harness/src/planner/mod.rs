//! The orchestrator planner: turn a task into a DAG of identity spawns
//! ([`Plan`]), audit every spawn before it runs, and dispatch the ones the
//! auditor allows, tracking the resulting child tasks.
//!
//! [`ModelPlanner`] proposes a [`Plan`] from a task description and an
//! [`IdentityCatalog`](crate::identity::IdentityCatalog); [`execute_plan`]
//! walks it in dependency order, running every [`SpawnNode`] through the
//! [`Gate`](crate::auditor::Gate) from `harness::auditor` before it may
//! dispatch, and skips the transitive dependents of anything the auditor
//! refuses.

mod execute;
mod model;
mod plan;

pub use execute::{execute_plan, NodeOutcome, PlanExecution, SpawnDispatcher};
pub use model::{ModelPlanner, Planner, OUTPUT_CONTRACT, PLANNER_FRAMING, TASK_CLOSE, TASK_OPEN};
pub use plan::{Plan, PlanError, PlanNodeId, SpawnNode};
