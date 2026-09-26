//! The orchestrator planner: turn a task into a DAG of identity spawns
//! ([`Plan`]).

mod plan;

pub use plan::{Plan, PlanError, PlanNodeId, SpawnNode};
