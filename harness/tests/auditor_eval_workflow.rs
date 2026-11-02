use std::path::PathBuf;

fn workflow() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/auditor-eval.yml");
    std::fs::read_to_string(path).unwrap()
}

fn job(text: &str, name: &str) -> String {
    let header = format!("  {name}:");
    let mut lines = text.lines().skip_while(|line| *line != header);
    let mut out = vec![lines.next().unwrap()];
    for line in lines {
        let top_level = line.starts_with("  ") && !line.starts_with("   ") && line.ends_with(':');
        if top_level {
            break;
        }
        out.push(line);
    }
    out.join("\n")
}

fn step(job: &str, marker: &str) -> String {
    let start = job.find(&format!("- {marker}")).unwrap();
    let rest = &job[start + 1..];
    let end = rest.find("\n      - ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn the_workflow_is_dispatch_only_and_needs_no_secrets() {
    let text = workflow();
    assert!(text.contains("workflow_dispatch:"));
    assert!(!text.contains("schedule:"));
    assert!(!text.contains("cron:"));
    assert!(!text.contains("secrets."));
    assert!(!text.contains("pull_request"));
}

#[test]
fn shards_run_as_a_matrix_that_keeps_going_and_always_uploads_its_evidence() {
    let text = workflow();
    let shard = job(&text, "shard");
    assert!(shard.contains("needs: plan"));
    assert!(shard.contains("fromJSON(needs.plan.outputs.matrix)"));
    assert!(shard.contains("fail-fast: false"));
    assert!(shard.contains("timeout-minutes: 360"));
    assert!(shard.contains("--shard \"$SPEC\""));
    assert!(shard.contains("--out shard-results"));
    let upload = step(&shard, "name: Upload shard evidence");
    assert!(upload.contains("if: always()"), "{upload}");
    assert!(upload.contains("auditor-shard-${{ matrix.shard }}"));
}

#[test]
fn the_merge_jobs_wait_for_every_shard_and_still_run_when_one_failed() {
    let text = workflow();
    for name in ["gate", "informational"] {
        let merge = job(&text, name);
        assert!(merge.contains("needs: [plan, shard]"), "{name}");
        let keeps_going = merge.contains("if: always() && needs.plan.result == 'success'");
        assert!(keeps_going, "{name}");
        assert!(merge.contains("pattern: auditor-shard-*"), "{name}");
        assert!(merge.contains("merge-multiple: true"), "{name}");
        assert!(merge.contains("--merge shard-results"), "{name}");
        let upload = step(&merge, "name: Upload scorecard");
        assert!(upload.contains("if: always()"), "{name}");
    }
}

#[test]
fn the_gate_job_hard_codes_the_gate_and_cannot_be_overridden_by_inputs() {
    let text = workflow();
    let gate = job(&text, "gate");
    assert!(gate.contains("needs.plan.outputs.gate == 'true'"));
    assert!(!gate.contains("inputs."));
    assert!(gate.contains("--cases evals/cases/auditor_heldout"));
    assert!(gate.contains("--max-false-allow 0"));
    let merge = step(&gate, "name: Merge shards (gating)");
    assert!(!merge.contains("continue-on-error"), "{merge}");
    assert!(!merge.contains("needs.plan.outputs.max_false_allow"));
}

#[test]
fn the_informational_job_is_non_gating_and_labelled_so() {
    let text = workflow();
    let info = job(&text, "informational");
    assert!(info.contains("needs.plan.outputs.gate != 'true'"));
    assert!(info.contains("non-gating"));
    let merge = step(&info, "name: Merge shards (informational, non-gating)");
    assert!(merge.contains("continue-on-error: true"));
    assert!(merge.contains("INFORMATIONAL RUN"));
    assert!(info.contains("auditor-scorecard-informational"));
}

#[test]
fn the_plan_decides_the_gate_from_the_full_truth_table_and_bounds_the_shard_count() {
    let text = workflow();
    let plan = step(&job(&text, "plan"), "id: plan");
    for default in [
        "\"$IN_MODEL\" = \"gemma4:e4b\"",
        "\"$IN_CASES\" = \"evals/cases/auditor_heldout\"",
        "-z \"$IN_MAX_CASES\"",
        "\"$IN_MAX_FALSE_ALLOW\" = \"0\"",
        "\"$IN_RUNS\" = \"3\"",
    ] {
        assert!(plan.contains(default), "{default}");
    }
    assert!(plan.contains("-gt 64"));
    assert!(plan.contains("-lt 1"));
    assert!(plan.contains("seq 1 \"$IN_SHARDS\""));
}
