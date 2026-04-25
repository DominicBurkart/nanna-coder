//! Evaluation reporting and analysis module.
//!
//! This module provides report generation for evaluation results produced
//! by the agent evaluation framework in [`crate::agent::eval`].

pub mod report;
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
