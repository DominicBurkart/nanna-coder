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
#[cfg(feature = "eval-runner")]
pub mod runner;
pub mod swebench;
pub mod swebench_report;
pub mod swebench_results;

// Re-export commonly used types from the agent eval module
pub use crate::agent::eval::{
    AgentEvaluationResult, BatchEvaluationResult, EvaluationCategory, EvaluationMetrics,
};
pub use swebench::{adapt_to_eval_case, load_swebench_dataset, materialize, SWEBenchTask};
pub use swebench_report::SweBenchReport;
pub use swebench_results::{
    SweBenchInstanceResult, SweBenchRunConfig, SweBenchRunResult, TokenUsage as SweBenchTokenUsage,
};

// Re-export runner types (only available with the eval-runner feature)
#[cfg(feature = "eval-runner")]
pub use runner::{run_eval, EvalRunResult, EvalRunnerConfig, EvalRunnerError};
