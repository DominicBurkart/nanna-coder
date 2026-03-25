//! Evaluation reporting and analysis module.
//!
//! This module provides report generation for evaluation results produced
//! by the agent evaluation framework in [`crate::agent::eval`].

pub mod report;

// Re-export commonly used types from the agent eval module
pub use crate::agent::eval::{
    AgentEvaluationResult, BatchEvaluationResult, EvaluationCategory, EvaluationMetrics,
};
