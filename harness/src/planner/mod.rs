//! The orchestrator planner: turn a task into a DAG of identity spawns
//! ([`Plan`]) via an LLM call over an [`IdentityCatalog`](crate::identity::IdentityCatalog)
//! ([`ModelPlanner`]).

mod model;
mod plan;

pub use model::{ModelPlanner, Planner, OUTPUT_CONTRACT, PLANNER_FRAMING, TASK_CLOSE, TASK_OPEN};
pub use plan::{Plan, PlanError, PlanNodeId, SpawnNode};
