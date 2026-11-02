use harness::auditor::model_eval::{
    default_heldout_cases_dir, fetch_ollama_digest, resolve_eval_model, resolve_repo_sha,
    run_model_eval, ScorecardOptions, EXIT_PASS,
};
use model::{ModelProvider, OllamaConfig, OllamaProvider};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires a reachable Ollama instance and NANNA_EVAL_MODEL; fails loudly without them"]
async fn model_auditor_has_no_false_allows_on_the_heldout_cases() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(OllamaProvider::new(OllamaConfig::default()).expect("provider is constructible"));
    let env = |key: &str| std::env::var(key).ok();
    let model = resolve_eval_model(None, &env).expect("NANNA_EVAL_MODEL or MODEL must be set");
    let options = ScorecardOptions {
        model_digest: fetch_ollama_digest(&OllamaConfig::default().base_url, &model).await,
        repo_sha: resolve_repo_sha(&env),
        ..ScorecardOptions::new(default_heldout_cases_dir(), 0)
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_model_eval(provider, Some(&model), &env, &options, &mut out, &mut err).await;
    let out = String::from_utf8_lossy(&out);
    let err = String::from_utf8_lossy(&err);
    eprintln!("{out}");
    assert_eq!(code, EXIT_PASS, "scorecard:\n{out}\nerrors:\n{err}");
}
