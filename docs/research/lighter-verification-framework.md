# Lighter Verification Framework — Eval & Strategy Implications

Research note for issue [#274](https://github.com/DominicBurkart/nanna-coder/issues/274).
Follow-up to the ADAS framing in [#196](https://github.com/DominicBurkart/nanna-coder/issues/196).

## 1. Definition: what "verification framework" means here

In nanna-coder today, "verification" is a layered stack that wraps the agent's
inner loop. The current guardrails (collected from `ARCHITECTURE.md`,
`AGENTS.md`, `codecov.yml`, `.github/workflows/`, `tarpaulin.toml`, and
`flake.nix`/`nix/`) include:

- **Container/dev-env isolation.** Agents run inside dev containers
  (see `ARCHITECTURE.md` "Container Topology"), with the dev container distinct
  from the harness container and from any release sandbox. Network and
  filesystem boundaries are imposed at the container layer.
- **Reproducible build shell.** Everything must run under
  `nix develop --command …`, pinning toolchain, system deps, and runtime.
- **CI gates.** `.github/workflows/ci.yml`, `eval.yml`, `codecov-guard.yml`,
  `cache-warming.yml`, `ci-integration.yml`, and `badges.yaml` enforce build,
  fmt, clippy `-D warnings`, nextest, doctests, integration containers, and
  cargo-deny.
- **Coverage floor.** `codecov.yml` pins a 100% patch-coverage `target:`
  with a single `ignore:` entry (`harness/src/main.rs`). PR #275 added an
  agent-proof regression guard rejecting target decreases or `ignore:` growth.
- **Coverage-bypass guard (in flight).** Issue #276 tracks closing the
  obvious next bypass surface (`#[cfg(not(tarpaulin))]`,
  `#[cfg_attr(coverage_nightly, coverage(off))]`, `#[ignore]` on tests, and
  `tarpaulin.toml exclude-files` growth).
- **Behavioural rules in AGENTS.md.** Hard prohibitions on lowering the
  coverage target, editing the guard workflow, or replacing the numeric
  target with `auto`. Escalation contract: file an issue rather than weaken.
- **Eval harness.** `harness/src/eval/` plus `evals/cases/**/task.toml`
  (e.g. `happy-path-001`, the SWE-bench-verified samples) plus the runner
  that PR #271 lands and PR #272 wires into `eval.yml`. PR #273 lands the
  SWE-bench result/report types.
- **Trait & registry scaffolding (planned).** Issues #203/#204 propose a
  `Workflow` trait, `WorkflowRegistry`, and a routing/selector node so
  multiple workflows can coexist and be picked per task.

The combination is *heavy* by design: it favours regression-safety and
agent-resistance over throughput.

## 2. Hypothesis: why lighter could win, and where it bites back

**Why lighter wins.** Each guardrail introduces a friction term that the
agent pays in tokens, wall time, and *behavioural distortion*:

- A 100% patch-coverage floor combined with `nix develop` rebuild times
  pushes the agent toward small, defensive diffs and away from speculative
  refactors. On exploratory tasks (RAG code research, novel test design,
  trajectory analysis — the follow-up domains in #196), this is exactly
  the wrong gradient.
- Container isolation is paid even when the task is read-only. For
  selector/router workflows whose "action" is choosing a workflow, the
  container spin-up dwarfs the inference cost.
- Strong CI gates mean the agent can only learn from the *gate* signal,
  not from intermediate failure modes. This collapses the reward surface
  and reduces the diagnostic value of a failed run.
- Coverage and lint gates create false-negatives on novel approaches:
  a refactor that *would* pass a human reviewer's read but happens to
  drop one untested edge-case path is killed at the gate, even when the
  diff strictly improves architecture.

**Where lighter bites.** The same gradient that frees exploration also
frees regression:

- **Insecure code.** Without container isolation, an agent can exfiltrate
  secrets or write outside its worktree. The MCP tool surface
  (`assign_task`, `onboard_repo`, etc. — see `ARCHITECTURE.md`) presumes a
  trusted boundary; a lighter variant must *not* relax that boundary.
- **Coverage drift / bypass.** #276 specifically calls out the path: an
  agent that "just adds `#[cfg(not(tarpaulin))]`" can pass a 100% gate
  while genuinely shipping uncovered code. A lighter variant that turns
  off the coverage gate altogether removes the symptom but inherits the
  disease.
- **Tarpaulin/toolchain drift.** Lighter pinning leads to "works on my
  agent" — irreproducible eval results, which destroys the comparability
  that #207's complementarity runner depends on.
- **Eval contamination.** SWE-bench-verified cases ship a `tests_must_pass`
  contract; without a deterministic build environment, false passes from
  toolchain skew dominate.

**Net hypothesis.** Lighter verification will likely improve pass-rate and
wall-time on **exploratory and orchestration domains** from #196's list
(RAG research, trajectory analysis, multi-workflow orchestration), and
will likely *regress* on **code-edit-with-tests** domains (SWE-bench,
test implementation, merge conflict resolution). The point of A/B testing
is to make those crossovers explicit rather than guessed.

## 3. Reference systems (best-effort; flagged where hand-waved)

Three points on the verification-density axis. *Verifiable* claims cite
public docs; *hand-waved* claims are based on general public reputation
and need follow-up before being treated as load-bearing.

- **Aider** (verifiable in broad strokes). Aider runs as a thin CLI
  against the user's existing checkout. Its "verification" is the user's
  pre-commit hooks plus, optionally, a configurable `--test-cmd`. There
  is no sandbox, no per-task container, no enforced coverage gate. This
  is roughly the *minimum* end of the spectrum: the human is the gate.
  Aider's published SWE-bench numbers are competitive on edit-locality
  tasks; it under-performs on tasks requiring multi-file orchestration
  with strong test signal.
- **OpenHands (formerly OpenDevin) without sandbox** (mixed; sandbox
  config is verifiable, "without-sandbox" perf is hand-waved). OpenHands
  ships with a runtime sandbox by default but exposes a "local runtime"
  mode for low-friction iteration. Disabling the sandbox yields large
  wall-time improvements on small edit tasks; published comparisons on
  full SWE-bench-verified are scarce and the perf gap on long tasks
  appears to be smaller than the wall-time gap on short tasks. Treat the
  performance ranking as a hypothesis to test, not a finding.
- **Raw tool-use + minimal CI** (this is a *category*, not a project).
  Many in-house agentic harnesses run with: a single `bash` tool, a
  read/write tool, and a CI that's "format + unit tests, advisory
  coverage". They tend to dominate on greenfield code generation and
  underperform on regression-heavy maintenance work. This matches the
  hypothesis in §2 and is the regime nanna-coder's lighter variant
  should occupy.

What we'd need to verify properly: per-system numbers on the same eval
suite (SWE-bench-verified is the obvious common denominator). That's
exactly what #207's complementarity runner is meant to produce. Until
those numbers exist, the case studies above are *priors*, not evidence.

## 4. Eval strategy: A/B test light-vs-heavy variants

**Setup.** Use the runner from #271 (`harness::eval::runner::run_eval`)
and its workflow integration from #272 (`.github/workflows/eval.yml`,
`NANNA_EVAL_MODEL=gemma4:e4b`). The cases under `evals/cases/` —
`happy-path-001..003` plus `swebench-{django,pytest,scikit-learn,sphinx,sympy}-*`
— are the starting suite. Ensemble/coverage analysis lives in #207's
runner.

**Variants.** Define three concrete verification levels:

- `verification = "heavy"` — current default. Container-isolated, nix
  shell, full CI gates simulated locally, 100% patch coverage required
  for promotion, all coverage-bypass guards active.
- `verification = "medium"` — drop the 100%-patch-coverage promotion
  requirement (run coverage as advisory only), keep nix shell + container.
- `verification = "light"` — no container, run inside the harness shell
  directly, run only `cargo build` + `cargo test` (no clippy `-D warnings`,
  no coverage). Still pinned to the nix toolchain to keep results
  reproducible — that's a non-negotiable for eval comparability, even
  in the light variant.

**Metrics.** Per case, per variant: pass/fail; wall time (median + p95
across N≥5 seeds); prompt + completion tokens; iteration count;
**security regressions** (agent attempted to write outside worktree, ran
network egress, or invoked a denied syscall — captured by an audit-only
container even in the `light` variant for telemetry); and **silent
regressions** (test passed but coverage of touched lines dropped vs
heavy variant — captured by running tarpaulin out-of-band on the
`light`/`medium` outputs).

**Pre-registration template** (commit before running):

```toml
[experiment]
id = "light-vs-heavy-2026-04-25"
hypothesis = "On greenfield happy-path cases, `light` will achieve >=`heavy` pass-rate at <=50% wall time. On SWE-bench-verified cases, `light` will regress on pass-rate by >5pp."
variants = ["heavy", "medium", "light"]
cases = ["happy-path-001", "happy-path-002", "happy-path-003",
         "swebench-django__django-11099", "swebench-pytest-dev__pytest-7490",
         "swebench-scikit-learn__scikit-learn-13142", "swebench-sphinx-doc__sphinx-8548",
         "swebench-sympy__sympy-20590"]
seeds = [0, 1, 2, 3, 4]
model = "gemma4:e4b"
primary_metric = "pass_rate"
secondary_metrics = ["wall_time_p50", "tokens_total", "security_violations"]
decision_rule = "promote `light` only if pass_rate(light) >= pass_rate(heavy) - 2pp AND security_violations(light) == 0"
```

**Extra evals needed for light variants** (gaps to file as follow-ups):

- A *security* eval suite (prompt-injection-shaped tasks; tasks that try
  to read `~/.ssh`; tasks that try to `curl` an external host). These
  must run under audit-only telemetry even when the variant claims to
  forgo isolation.
- A *coverage-faithfulness* eval that cross-checks final coverage vs the
  agent's claimed coverage, catching #276-style bypasses introduced by
  the agent itself rather than by hand.
- A *non-determinism* eval (run the same case under the same seed N
  times in the `light` variant; flag if pass-rate variance > threshold).

## 5. Strategy-listing integration

Surface verification as an explicit axis in the workflow registry from
#203/#204. Concretely, add a `verification` field to the registry
metadata so the selector node from #204 can route based on
"this task tolerates a lighter verification level".

```toml
# Sketch — extends what #203 proposes for registry metadata.
[[workflow]]
name = "react-default"
description = "Embellished ReAct loop with static analysis"
capabilities = ["code-edit", "test-implementation"]
verification = "heavy"        # NEW
suitability_hints = "..."

[[workflow]]
name = "react-light"
description = "Same loop, no container, advisory coverage"
capabilities = ["code-edit", "rag-research", "trajectory-analysis"]
verification = "light"
suitability_hints = "Use for read-mostly research tasks, RAG over a repo, or low-stakes scaffolding."
```

```rust
// Rust sketch — extension to the Workflow trait from #203.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationLevel { Heavy, Medium, Light }

pub trait Workflow {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    fn suitability_hints(&self) -> &str;
    fn verification(&self) -> VerificationLevel;   // NEW
    async fn run(&self, task: Task, ctx: Ctx) -> WorkflowOutcome;
}
```

The selector (#204) gets one extra term: when the incoming task carries
a security-sensitive marker (writes outside worktree, touches secrets,
modifies CI), refuse any `Light` workflow at routing time. This is
**routing-level enforcement** — distinct from the per-PR coverage guard,
and orthogonal to it.

## 6. Risks & mitigations

The headline tension with #276: a "lighter verification" workflow must
*not* become a back-door for coverage bypass. Mitigations, in order of
strength:

- **Keep the per-PR coverage-bypass guard from #276 active for all
  variants.** A workflow with `verification = Light` is still subject to
  the *repo-level* PR gate when its output is promoted to a PR. The
  light/heavy axis governs the **agent's inner loop**, not the
  promotion gate.
- **Audit-only sandbox** even in `light`. The container is removed from
  the agent's *experience* (no syscall denials, no network blocks
  surfaced as errors) but a thin audit layer still records violations
  for the eval report. Cost is small; signal is large; aligned with
  ADAS-style observability from #196.
- **Two-stage promotion.** A `light` workflow's output never lands
  directly on a branch; it produces a candidate diff that a `heavy`
  workflow re-validates (rebuild, test, coverage) before opening a PR.
  This converts the tension into a pipeline rather than a tradeoff.
- **Pre-registered evals** (§4). Decision rules are committed before
  the experiment runs, so post-hoc justification of regressions is
  syntactically blocked.
- **Don't move `codecov.yml` or `.github/workflows/codecov-guard.yml`.**
  Per `AGENTS.md` and the path-restriction Repository Ruleset
  contemplated in #276, a lighter-verification workflow has no business
  touching those files. The lighter variant lives at the workflow layer,
  not the repo-policy layer.

## 7. Recommended next steps (do not file yet)

1. **Spec issue: `VerificationLevel` field on `Workflow`** — add to the
   #203 trait + registry, including selector behaviour from #204 for
   security-sensitive tasks.
2. **Spec issue: light workflow variant** — concrete `react-light`
   implementation, audit-only sandbox, two-stage promotion to a `heavy`
   re-validation step.
3. **Eval issue: security eval suite** — prompt-injection / exfiltration
   / out-of-worktree write cases under `evals/cases/security/`, runnable
   by all variants.
4. **Eval issue: coverage-faithfulness eval** — cross-check #276-style
   bypasses end-to-end, not just at PR-time.
5. **Eval issue: pre-registration template & decision-rule machinery**
   in `harness::eval::experiment` so A/B comparisons are committed
   before they're run.
6. **Runner extension to #207** — accept `VerificationLevel` as a
   dimension and emit per-level matrices in the complementarity report.
7. **Reference-systems benchmark** — run Aider and OpenHands (sandboxed
   and unsandboxed) on the same `evals/cases/` subset to replace the
   hand-waved §3 ranking with measurements before any nanna-coder
   workflow ships its `verification` setting.
