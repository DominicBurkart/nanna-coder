//! Shellout to the upstream SWE-bench evaluation harness.
//!
//! The agent-side runner produces a unified-diff patch per instance; this
//! module is responsible for running those patches through
//! `python -m swebench.harness.run_evaluation` (which spins up a per-instance
//! Docker container, applies the patch, runs the project's test suite, and
//! emits a per-instance `report.json`) and ingesting the verdicts back into
//! the [`SweBenchInstanceResult`] shape we report on.
//!
//! Only the `verify_predictions` entrypoint touches Python/Docker; all other
//! helpers are pure I/O over filesystem paths and are unit-testable without
//! the upstream harness installed.
//!
//! ```rust,no_run
//! use harness::eval::swebench_verify::{verify_predictions, Prediction, VerifyConfig};
//! use std::path::PathBuf;
//!
//! # async fn example() -> Result<(), harness::eval::swebench_verify::VerifyError> {
//! let predictions = vec![Prediction {
//!     instance_id: "django__django-11099".to_string(),
//!     model_patch: "diff --git ...".to_string(),
//! }];
//! let config = VerifyConfig {
//!     dataset_name: "princeton-nlp/SWE-bench_Verified".to_string(),
//!     model_name_or_path: "nanna__gemma4-e4b".to_string(),
//!     run_id: "run-001".to_string(),
//!     work_dir: PathBuf::from("/tmp/swe-verify"),
//!     max_workers: 4,
//! };
//! let verdicts = verify_predictions(&predictions, &config).await?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// A single agent prediction submitted to the upstream harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub instance_id: String,
    pub model_patch: String,
}

/// Configuration for a verification run.
#[derive(Debug, Clone)]
pub struct VerifyConfig {
    pub dataset_name: String,
    pub model_name_or_path: String,
    pub run_id: String,
    pub work_dir: PathBuf,
    pub max_workers: usize,
}

/// The verdict for a single instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceVerdict {
    pub instance_id: String,
    pub resolved: bool,
    pub error: Option<String>,
}

/// Errors returned by the verifier.
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize predictions: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to parse upstream report at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("upstream harness exited with status {status}: {stderr}")]
    HarnessExit {
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("failed to spawn upstream harness ({0}): {1}")]
    Spawn(String, #[source] std::io::Error),
    #[error("no per-instance reports found under {0}")]
    NoReports(PathBuf),
}

pub fn predictions_path(work_dir: &Path) -> PathBuf {
    work_dir.join("predictions.json")
}

pub fn sanitize_model_name(model_name_or_path: &str) -> String {
    model_name_or_path.replace('/', "__")
}

pub fn run_evaluation_dir(work_dir: &Path, run_id: &str, model_name_or_path: &str) -> PathBuf {
    work_dir
        .join("logs")
        .join("run_evaluation")
        .join(run_id)
        .join(sanitize_model_name(model_name_or_path))
}

pub fn write_predictions(
    predictions: &[Prediction],
    config: &VerifyConfig,
) -> Result<PathBuf, VerifyError> {
    std::fs::create_dir_all(&config.work_dir).map_err(|source| VerifyError::Io {
        path: config.work_dir.clone(),
        source,
    })?;
    let path = predictions_path(&config.work_dir);

    #[derive(Serialize)]
    struct Record<'a> {
        instance_id: &'a str,
        model_patch: &'a str,
        model_name_or_path: &'a str,
    }
    let records: Vec<Record<'_>> = predictions
        .iter()
        .map(|p| Record {
            instance_id: &p.instance_id,
            model_patch: &p.model_patch,
            model_name_or_path: &config.model_name_or_path,
        })
        .collect();

    let json = serde_json::to_string_pretty(&records).map_err(VerifyError::Serialize)?;
    std::fs::write(&path, json).map_err(|source| VerifyError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

pub fn parse_evaluation_results(
    work_dir: &Path,
    run_id: &str,
    model_name_or_path: &str,
) -> Result<Vec<InstanceVerdict>, VerifyError> {
    let root = run_evaluation_dir(work_dir, run_id, model_name_or_path);
    if !root.is_dir() {
        return Err(VerifyError::NoReports(root));
    }

    let mut verdicts = Vec::new();
    let entries = std::fs::read_dir(&root).map_err(|source| VerifyError::Io {
        path: root.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| VerifyError::Io {
            path: root.clone(),
            source,
        })?;
        let instance_dir = entry.path();
        if !instance_dir.is_dir() {
            continue;
        }
        let report_path = instance_dir.join("report.json");
        if !report_path.is_file() {
            continue;
        }
        verdicts.push(parse_instance_report(&report_path)?);
    }
    verdicts.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    Ok(verdicts)
}

#[derive(Deserialize, Debug)]
struct UpstreamReport {
    #[serde(default)]
    resolved: bool,
    #[serde(default, rename = "patch_is_None")]
    patch_is_none: bool,
    #[serde(default)]
    patch_exists: bool,
    #[serde(default)]
    patch_successfully_applied: bool,
}

fn parse_instance_report(path: &Path) -> Result<InstanceVerdict, VerifyError> {
    let content = std::fs::read_to_string(path).map_err(|source| VerifyError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let outer: std::collections::BTreeMap<String, UpstreamReport> = serde_json::from_str(&content)
        .map_err(|source| VerifyError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    let (instance_id, report) = outer.into_iter().next().ok_or_else(|| VerifyError::Parse {
        path: path.to_path_buf(),
        source: serde::de::Error::custom("empty report.json object"),
    })?;

    let error = if report.patch_is_none {
        Some("agent produced no patch".to_string())
    } else if !report.patch_exists {
        Some("patch missing in submission".to_string())
    } else if !report.patch_successfully_applied {
        Some("patch failed to apply".to_string())
    } else {
        None
    };

    Ok(InstanceVerdict {
        instance_id,
        resolved: report.resolved,
        error,
    })
}

pub async fn verify_predictions(
    predictions: &[Prediction],
    config: &VerifyConfig,
) -> Result<Vec<InstanceVerdict>, VerifyError> {
    let preds_path = write_predictions(predictions, config)?;

    let status = tokio::process::Command::new("python")
        .arg("-m")
        .arg("swebench.harness.run_evaluation")
        .arg("--predictions_path")
        .arg(&preds_path)
        .arg("--dataset_name")
        .arg(&config.dataset_name)
        .arg("--max_workers")
        .arg(config.max_workers.to_string())
        .arg("--run_id")
        .arg(&config.run_id)
        .current_dir(&config.work_dir)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .output()
        .await
        .map_err(|e| {
            VerifyError::Spawn("python -m swebench.harness.run_evaluation".to_string(), e)
        })?;

    if !status.status.success() {
        return Err(VerifyError::HarnessExit {
            status: status.status,
            stderr: String::from_utf8_lossy(&status.stderr).to_string(),
        });
    }

    parse_evaluation_results(&config.work_dir, &config.run_id, &config.model_name_or_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_predictions() -> Vec<Prediction> {
        vec![
            Prediction {
                instance_id: "django__django-11099".to_string(),
                model_patch:
                    "diff --git a/x.py b/x.py\n--- a/x.py\n+++ b/x.py\n@@ -1 +1 @@\n-old\n+new\n"
                        .to_string(),
            },
            Prediction {
                instance_id: "pytest-dev__pytest-7490".to_string(),
                model_patch: "".to_string(),
            },
        ]
    }

    fn sample_config(work_dir: &Path) -> VerifyConfig {
        VerifyConfig {
            dataset_name: "princeton-nlp/SWE-bench_Verified".to_string(),
            model_name_or_path: "nanna/gemma4-e4b".to_string(),
            run_id: "test-run-001".to_string(),
            work_dir: work_dir.to_path_buf(),
            max_workers: 1,
        }
    }

    #[test]
    fn predictions_path_is_under_work_dir() {
        let p = predictions_path(Path::new("/tmp/foo"));
        assert_eq!(p, PathBuf::from("/tmp/foo/predictions.json"));
    }

    #[test]
    fn sanitize_replaces_slashes_with_double_underscore() {
        assert_eq!(sanitize_model_name("nanna/gemma4-e4b"), "nanna__gemma4-e4b");
        assert_eq!(sanitize_model_name("plain"), "plain");
        assert_eq!(sanitize_model_name("a/b/c"), "a__b__c");
    }

    #[test]
    fn run_evaluation_dir_matches_upstream_layout() {
        let p = run_evaluation_dir(Path::new("/work"), "run-001", "nanna/gemma4-e4b");
        assert_eq!(
            p,
            PathBuf::from("/work/logs/run_evaluation/run-001/nanna__gemma4-e4b")
        );
    }

    #[test]
    fn write_predictions_creates_work_dir_and_writes_records() {
        let dir = tempdir().unwrap();
        let work = dir.path().join("nested").join("work");
        let cfg = sample_config(&work);

        let path = write_predictions(&sample_predictions(), &cfg).unwrap();

        assert_eq!(path, predictions_path(&work));
        assert!(path.is_file(), "predictions.json should exist");

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let arr = parsed.as_array().expect("predictions.json should be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["instance_id"], "django__django-11099");
        assert_eq!(arr[0]["model_name_or_path"], "nanna/gemma4-e4b");
        assert!(arr[0]["model_patch"]
            .as_str()
            .unwrap()
            .contains("diff --git"));
        assert_eq!(arr[1]["instance_id"], "pytest-dev__pytest-7490");
        assert_eq!(arr[1]["model_patch"], "");
    }

    fn fixture_report_tree(
        work_dir: &Path,
        run_id: &str,
        model_name_or_path: &str,
        reports: &[(&str, serde_json::Value)],
    ) -> String {
        let sanitised = sanitize_model_name(model_name_or_path);
        let model_dir = work_dir
            .join("logs")
            .join("run_evaluation")
            .join(run_id)
            .join(&sanitised);
        for (instance_id, body) in reports {
            let dir = model_dir.join(instance_id);
            std::fs::create_dir_all(&dir).unwrap();
            let mut outer = serde_json::Map::new();
            outer.insert((*instance_id).to_string(), body.clone());
            std::fs::write(
                dir.join("report.json"),
                serde_json::to_string_pretty(&serde_json::Value::Object(outer)).unwrap(),
            )
            .unwrap();
        }
        sanitised
    }

    #[test]
    fn parse_evaluation_results_picks_up_reports_and_sorts() {
        let dir = tempdir().unwrap();
        let reports: &[(&str, serde_json::Value)] = &[
            (
                "django__django-11099",
                serde_json::json!({
                    "resolved": true,
                    "patch_is_None": false,
                    "patch_exists": true,
                    "patch_successfully_applied": true,
                }),
            ),
            (
                "sympy__sympy-20590",
                serde_json::json!({
                    "resolved": false,
                    "patch_is_None": false,
                    "patch_exists": true,
                    "patch_successfully_applied": true,
                }),
            ),
        ];
        fixture_report_tree(dir.path(), "run-A", "nanna/gemma4-e4b", reports);

        let verdicts = parse_evaluation_results(dir.path(), "run-A", "nanna/gemma4-e4b").unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].instance_id, "django__django-11099");
        assert!(verdicts[0].resolved);
        assert!(verdicts[0].error.is_none());
        assert_eq!(verdicts[1].instance_id, "sympy__sympy-20590");
        assert!(!verdicts[1].resolved);
        assert!(verdicts[1].error.is_none());
    }

    #[test]
    fn parse_evaluation_results_surfaces_patch_failure_modes() {
        let dir = tempdir().unwrap();
        let reports: &[(&str, serde_json::Value)] = &[
            (
                "no_patch",
                serde_json::json!({
                    "resolved": false,
                    "patch_is_None": true,
                    "patch_exists": false,
                    "patch_successfully_applied": false,
                }),
            ),
            (
                "missing_patch",
                serde_json::json!({
                    "resolved": false,
                    "patch_is_None": false,
                    "patch_exists": false,
                    "patch_successfully_applied": false,
                }),
            ),
            (
                "apply_failed",
                serde_json::json!({
                    "resolved": false,
                    "patch_is_None": false,
                    "patch_exists": true,
                    "patch_successfully_applied": false,
                }),
            ),
        ];
        fixture_report_tree(dir.path(), "run-B", "model", reports);

        let verdicts = parse_evaluation_results(dir.path(), "run-B", "model").unwrap();
        let by_id: std::collections::HashMap<_, _> = verdicts
            .iter()
            .map(|v| (v.instance_id.as_str(), v))
            .collect();

        assert_eq!(
            by_id["no_patch"].error.as_deref(),
            Some("agent produced no patch")
        );
        assert_eq!(
            by_id["missing_patch"].error.as_deref(),
            Some("patch missing in submission")
        );
        assert_eq!(
            by_id["apply_failed"].error.as_deref(),
            Some("patch failed to apply")
        );
        for v in &verdicts {
            assert!(!v.resolved);
        }
    }

    #[test]
    fn parse_evaluation_results_errors_when_root_missing() {
        let dir = tempdir().unwrap();
        let err = parse_evaluation_results(dir.path(), "no-run", "any").unwrap_err();
        assert!(matches!(err, VerifyError::NoReports(_)));
    }

    #[test]
    fn parse_evaluation_results_skips_dirs_without_report_json() {
        let dir = tempdir().unwrap();
        let model_dir = run_evaluation_dir(dir.path(), "run-C", "model");
        std::fs::create_dir_all(model_dir.join("instance_with_report")).unwrap();
        std::fs::create_dir_all(model_dir.join("instance_without_report")).unwrap();
        let outer = serde_json::json!({
            "instance_with_report": {
                "resolved": true,
                "patch_is_None": false,
                "patch_exists": true,
                "patch_successfully_applied": true,
            }
        });
        std::fs::write(
            model_dir.join("instance_with_report").join("report.json"),
            serde_json::to_string(&outer).unwrap(),
        )
        .unwrap();

        let verdicts = parse_evaluation_results(dir.path(), "run-C", "model").unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].instance_id, "instance_with_report");
    }

    #[test]
    fn parse_evaluation_results_propagates_bad_json() {
        let dir = tempdir().unwrap();
        let model_dir = run_evaluation_dir(dir.path(), "run-D", "model");
        std::fs::create_dir_all(model_dir.join("broken")).unwrap();
        std::fs::write(model_dir.join("broken").join("report.json"), "{not json").unwrap();

        let err = parse_evaluation_results(dir.path(), "run-D", "model").unwrap_err();
        assert!(matches!(err, VerifyError::Parse { .. }));
    }
}
