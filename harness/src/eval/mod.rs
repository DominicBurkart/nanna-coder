//! Evaluation reporting, analysis, and execution module.
//!
//! This module provides:
//! - **Report generation** for evaluation results ([`report`])
//! - **Eval runner** for executing single eval cases against the agent ([`runner`])
//!
//! The evaluation framework in [`crate::agent::eval`] provides the underlying
//! types, while [`runner`] bridges [`crate::agent::eval_case::EvalCase`] with
//! the [`crate::agent::AgentLoop`].

pub mod report;
pub mod runner;

// Re-export commonly used types from the agent eval module
pub use crate::agent::eval::{
    AgentEvaluationResult, BatchEvaluationResult, EvaluationCategory, EvaluationMetrics,
};

// Re-export runner types
pub use runner::{run_eval, EvalRunResult, EvalRunnerConfig, EvalRunnerError};
