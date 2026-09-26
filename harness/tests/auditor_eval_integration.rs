//! Scores a model-backed [`ModelAuditor`](harness::auditor::ModelAuditor)
//! against the `evals/cases/auditor_spawn/` cases.
//!
//! Requires a reachable Ollama instance and a tool-capable model named by
//! `NANNA_EVAL_MODEL` (or `MODEL`), following the gating in
//! `harness/src/bin/eval.rs`. Run with:
//! `NANNA_EVAL_MODEL=<model> cargo test --test auditor_eval_integration -- --ignored`

use harness::auditor::eval::{default_cases_dir, default_catalog_dir, score, AuditorEvalCase};
use harness::auditor::{AuditContext, ModelAuditor};
use harness::identity::IdentityCatalog;
use model::{ModelProvider, OllamaConfig, OllamaProvider};
use std::sync::Arc;

fn resolve_model() -> Option<String> {
    std::env::var("NANNA_EVAL_MODEL")
        .ok()
        .or_else(|| std::env::var("MODEL").ok())
}

#[tokio::test]
#[ignore = "requires a reachable Ollama instance and NANNA_EVAL_MODEL"]
async fn model_auditor_scores_the_shipped_cases() {
    let Some(model) = resolve_model() else {
        eprintln!("model_auditor_scores_the_shipped_cases: NANNA_EVAL_MODEL not set; skipping");
        return;
    };
    let provider: Arc<dyn ModelProvider> = match OllamaProvider::new(OllamaConfig::default()) {
        Ok(provider) => Arc::new(provider),
        Err(e) => {
            eprintln!(
                "model_auditor_scores_the_shipped_cases: provider unavailable ({e}); skipping"
            );
            return;
        }
    };

    let cases = AuditorEvalCase::discover(&default_cases_dir()).expect("shipped cases parse");
    let catalog = IdentityCatalog::load(default_catalog_dir()).expect("shipped catalog loads");
    let auditor_identity = catalog
        .get("auditor")
        .expect("catalog has an auditor card")
        .clone();
    let context = AuditContext::new(catalog, auditor_identity).expect("auditor identity is inert");
    let auditor = ModelAuditor::new(provider, model);

    let (outcomes, summary) = score(&auditor, &context, &cases)
        .await
        .expect("scoring completes");
    eprintln!("auditor_spawn eval summary: {summary:?}");
    for outcome in &outcomes {
        eprintln!(
            "{}: expected {:?}, got {:?}, passed={}",
            outcome.case_id, outcome.expected, outcome.actual, outcome.passed
        );
    }
    assert_eq!(summary.total, cases.len());
}
