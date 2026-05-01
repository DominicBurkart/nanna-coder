# Lighter Verification Framework — Eval & Strategy Implications

Research note for issue [#274](https://github.com/DominicBurkart/nanna-coder/issues/274).
Follow-up to the ADAS framing in [#196](https://github.com/DominicBurkart/nanna-coder/issues/196).

> **Post-review reframing (2026-05).** §§1–6 conflate two different
> things under one heading: **(A)** nanna-coder's own CI/CD — the gates
> that protect nanna-coder *as a project being built and released*
> (codecov 100%, the #275/#276 coverage-bypass guards, AGENTS.md hard
> rules, `nix develop` pinning, the `.github/workflows/` gates) — and
> **(B)** the per-turn verification Nanna applies to its own
> intermediate outputs while acting as a coding agent on a user task.
> The owner's review on
> [#277](https://github.com/DominicBurkart/nanna-coder/pull/277) is
> entirely about (B); the right reframing of "lighter verification" is
> **domain-specific per-turn verification** within (B). (A) is invariant
> under task domain — you do not lower the codecov floor on nanna-coder
> because Nanna happens to be working on a SQL task. §7 develops the
> distinction in full and §8 (follow-ups) is rewritten against it; the
> original §§1–6 are retained as the prior, conflated analysis they
> were reviewed against.

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
- **Complementarity runner (planned).** Issue #207 proposes an ensemble
  runner that executes the same case across multiple workflows/models and
  emits a complementarity matrix (which configs cover cases the others
  miss). §3, §4, and §7 below all assume this runner as the substrate
  for cross-variant comparison.

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
- Coverage and lint gates create false rejections on novel approaches
  (in standard test-theory framing this is a false *positive* — positive
  means "rejected as bad" — phrased here as "false rejections" to avoid
  the ambiguity for an audience of coding agents): a refactor that
  *would* pass a human reviewer's read but happens to drop one untested
  edge-case path is killed at the gate, even when the diff strictly
  improves architecture.

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
  Aider's published SWE-bench numbers are competitive (historically
  top-tier on the SWE-bench Verified leaderboard, including multi-file
  edits); per-domain breakdowns by orchestration depth are not, to our
  knowledge, published — treat any orchestration-vs-edit-locality split
  as a hypothesis to test rather than a finding.
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
which currently defaults to `model: qwen3:0.6b` per the workflow's
`workflow_dispatch.inputs.model.default`; pre-registration must read
the actual default at the time the experiment is committed, *not* the
placeholder used in the prior draft of this note). The cases under
`evals/cases/` —
`happy-path-001..003` plus `swebench-{django,pytest,scikit-learn,sphinx,sympy}-*`
— are the starting suite. Ensemble/coverage analysis lives in #207's
runner.

**Variants.** Define three concrete verification levels:

- `verification = "heavy"` — current default. Container-isolated, nix
  shell, full CI gates simulated locally, 100% patch coverage required
  for promotion, all coverage-bypass guards active.
- `verification = "medium"` — drop the 100%-patch-coverage promotion
  requirement (run coverage as advisory only), keep nix shell + container.
- `verification = "light"` — no *blocking* container around the agent's
  inner loop; the agent runs inside the harness shell directly and sees
  no syscall denials or network blocks as errors. An **audit-only**
  container wrapper is still present (see §6) recording security
  violations for the eval report — this is non-blocking telemetry, not
  enforcement. Within `light`, only `cargo build` + `cargo test` are
  required (no clippy `-D warnings`, no coverage). Still pinned to the
  nix toolchain to keep results reproducible — that's a non-negotiable
  for eval comparability, even in the light variant.

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
model = "qwen3:0.6b"  # match `.github/workflows/eval.yml` default at commit time
primary_metric = "pass_rate"
secondary_metrics = ["wall_time_p50", "tokens_total", "security_violations"]
decision_rule = "promote `light` only if pass_rate(light) >= pass_rate(heavy) - 2pp AND security_violations(light) == 0"
# NOTE: `model` above MUST be set from the live default of
# `.github/workflows/eval.yml` (workflow_dispatch.inputs.model.default)
# at commit time. As of 2026-04, that default is `qwen3:0.6b`.
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

A second axis — `domain` — is added to anticipate §7's reframing: the
selector should pick a workflow whose verification toolset is
appropriate to the task's domain, not just whose verification level is
"low enough to be fast". `domain = "rust-nix"` is the in-scope default
today; other values (`"sql-generation"`, `"natural-language"`,
`"mechanical-edit"`) are placeholders for the domain-specific workflows
§7 anticipates.

**Scope note.** The `verification` and `domain` fields below describe
Nanna's *per-turn verification while acting as a coding agent on a user
task* (the (B) layer in the top-of-doc note). They do **not** describe
the gates that protect nanna-coder itself: codecov, the
coverage-bypass guard, AGENTS.md hard rules, and the
`.github/workflows/` gates apply to every PR landed in nanna-coder
regardless of which workflow produced it and which domain that workflow
serves.

```toml
# Sketch — extends what #203 proposes for registry metadata.
[[workflow]]
name = "react-default"
description = "Embellished ReAct loop with static analysis"
capabilities = ["code-edit", "test-implementation"]
domain = "rust-nix"           # NEW — see §7
verification = "heavy"        # NEW
suitability_hints = "..."

[[workflow]]
name = "react-light"
description = "Same loop, no container, advisory coverage"
capabilities = ["code-edit", "rag-research", "trajectory-analysis"]
domain = "rust-nix"
verification = "light"
suitability_hints = "Use for read-mostly research tasks, RAG over a repo, or low-stakes scaffolding."

[[workflow]]
name = "ast-mechanical"
description = "Deterministic AST transform; verification is the tool's own contract"
capabilities = ["mechanical-edit"]
domain = "mechanical-edit"
verification = "none"          # see §7: zero-verification path
suitability_hints = "Use when the orchestrator has already decomposed the task to a single deterministic call."
```

```rust
// Rust sketch — extension to the Workflow trait from #203.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationLevel { None, Heavy, Medium, Light }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Domain(pub String); // e.g. "rust-nix", "sql-generation", "mechanical-edit"

pub trait Workflow {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    fn suitability_hints(&self) -> &str;
    fn domain(&self) -> &Domain;                   // NEW — see §7
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

## 7. Reframing: domain-specific per-turn verification

§§1–6 conflate two layers; §7 separates them and applies the
reframing to one of them.

### 7.1 Two layers, not one

- **(A) nanna-coder's CI/CD.** The gates that protect nanna-coder *as
  a project being built and released*. Concretely: codecov 100% patch
  coverage, the #275/#276 coverage-bypass guards, the hard rules in
  `AGENTS.md`, the `nix develop` pinning, and the
  `.github/workflows/` gates (`ci.yml`, `eval.yml`, `codecov-guard.yml`,
  etc.). These run on every PR opened against nanna-coder, regardless
  of which workflow produced the diff and regardless of what user task
  triggered the work.
- **(B) Nanna's per-turn verification while acting as a coding agent
  on a user task.** What Nanna runs on its own intermediate outputs
  *inside the agent loop*: which checks it consults, which signals it
  feeds back into the next turn, what it spends tokens validating
  before committing to a next step.

§§1–6 list (A) items (codecov, coverage guards, AGENTS.md, nix
shell, GitHub Action gates) and treat them as if they parameterise
(B). They do not. (A) is invariant under task domain. The reframing
below applies to (B) only.

### 7.2 The reframing, scoped to (B)

The owner's review on PR #277 argues that within (B), `heavy ↔ light`
is a proxy for the real axis: **domain-specific per-turn
verification**. Lighter (B)-verification is genuinely the right call
in two cases only:

1. **Validation deferred to CI** — either the user-project's CI or, if
   Nanna is editing nanna-coder itself, layer (A). The agent's inner
   loop skips a check that the project's own merge gate will run
   anyway; net signal is unchanged. This is what motivates the §4
   `light` variant when the eval target is nanna-coder.
2. **Domains that are not meaningfully decomposable.** If the task
   cannot be split into sub-domains with their own validation
   primitives, per-step verification has nothing to attach to and
   "lighter at the outer level" is the only knob. Most tasks are not
   in this bucket.

For everything else within (B), "lighter" is the wrong frame. The
right frame is **decomposition + per-sub-domain verification**: split
the task into sub-domains whose outputs can be validated with the
toolset that suits each one, then route each sub-step to a workflow
specialised for that sub-domain. Nanna's job within (B) is
"decomposition + domain-specific verification" rather than "ReAct with
a single verification dial".

### 7.3 Why a single dial is a poor (B) primitive

The set of useful per-turn verifications depends on the task's domain.
"Software development" is not one domain with one verification toolset.
Nanna's current harness, prompts, and tool surface are specialised for
rust+nix codebases; its per-turn verification primitives (run `cargo
build`, run `cargo test`, parse `clippy` output, query the rustc type
checker through the LSP) are calibrated for that domain. Reusing those
primitives wholesale on a non-rust task either under-validates (wrong
primitives — a Python type-check tells us nothing about borrow-checker
invariants, and vice versa) or over-validates (wasting tokens
collecting signal that doesn't bear on the task domain).

Note this is a separate concern from layer (A). When Nanna is asked
to do work on a non-rust user codebase, layer (A) doesn't even apply
to that work — (A) only fires when Nanna's output lands as a PR
against nanna-coder. The mismatch is purely a (B) problem: Nanna's
per-turn verification primitives are domain-mismatched.

### 7.4 Worked example: NL → SQL (a (B) decomposition)

Generating 500 lines of correct SQL from a natural-language question
requires both (a) a correct semantic model (the intent maps to
relationships expressible in the target schema) and (b) syntactically
valid SQL against that schema. Trying to do both in one pass and then
"verify with `cargo build`" makes no sense — there's no rust to
build, and the failure modes have nothing to do with what the
rust+nix per-turn primitives can detect. The domain-specific
decomposition:

- **Step 1 — input filtering** (deterministic / classifier). Reject
  off-domain or adversarial inputs before they reach a model. *Cost:
  near-zero. Per-turn verification: the filter's own contract.*
- **Step 2 — NL → structure-specific intermediate** (LLM-as-judge for
  semantics). Translate the natural-language question into a
  schema-aware intermediate representation. Per-turn verification via:
  domain alignment (does the output describe relationships expressible
  in this schema?), inversion (does an "agnosticator" LLM round-trip
  the intermediate back to a paraphrase of the input?), internal
  consistency (formalise the relationships and check invariants — a
  LEAN proof can identify semantic failures and drive iteration).
- **Step 3 — intermediate → SQL** (deterministic). Generate SQL from
  the intermediate. Per-turn verification via: parse the SQL,
  type-check it against the live schema, dry-run with `EXPLAIN`.
  *Cost: low. Verification: fully deterministic.*

This is *more* per-turn verification than nanna-coder's current
rust+nix-specialised loop applies, not less — but each piece is
matched to its sub-domain. Code verification is irrelevant while
validating an NL-to-language conversion, *unless* a sub-step
decomposes into a code-expressible domain. "Is this system design or
prompt engineering?" — under the ADAS framing in #196 there isn't a
meaningful difference; Nanna is formalising and validating its chain
of thought through specialised agents.

### 7.5 Worked example: mechanical AST transform (a (B) zero case)

When the orchestrator has already decomposed a task to "rename `AAA`
to `BBB` via an AST transform", the work delegated to the inner step
is a single call to a fully deterministic, well-tested tool. The right
per-turn verification is *essentially nothing*: "I called the tool
with the requested parameters and it returned success." Anything more
is wasted tokens. If a cosmic-ray-class anomaly does corrupt the
result, the orchestrator re-issues the call. This is the case for
`verification = "none"` in the §5 registry sketch.

(A) is unaffected: if this AST transform produces a diff against
nanna-coder, the diff still has to clear codecov, the
coverage-bypass guard, and the rest of nanna-coder's CI. The (B)
zero-verification setting is about not paying tokens for
agent-internal checks the deterministic tool already implies — not
about loosening the repo's release gates.

### 7.6 Actionable framing

"Remove domain-irrelevant per-turn verification strategies" is more
actionable than "lighter verification". It tells the registry, the
selector, and the eval suite what to *do*: identify the task's
domain, select the workflow specialised for it, validate per-turn
with the primitives that fit, and don't pay tokens for primitives
that don't. Layer (A) sits underneath all of this and is unchanged.

### 7.7 What this implies for §§4 and §5

- **§4's variants conflate (A) and (B).** The `heavy` variant lists
  "100% patch coverage required for promotion" (an (A) thing) next to
  "container-isolated agent loop" (a (B) thing). The actually-useful
  A/B test holds (A) constant — every variant produces output that
  must clear nanna-coder's CI to land — and varies (B): does the
  workflow's per-turn loop run inside a container, does it consult
  `cargo test` on every iteration, does it consult coverage at all
  during the loop. Re-read §4 with that scope restriction; the §4
  pre-registration template is still usable but the variant
  definitions need rewriting before any pre-registration commits.
- **§5's `verification` and `domain` are (B) fields.** They describe
  per-turn verification within a workflow, not the gates that promote
  the workflow's output. The selector (#204) routes on (B); the
  promotion gate is (A) and is invariant.

## 8. Recommended next steps (do not file yet)

Reorganised against the §7 reframing. Each item is tagged (A) or (B)
to make the layer explicit; items in the prior draft are preserved
where the work is still required, with notes when their scope was
narrowed by disentangling the layers.

1. **(B) Spec issue: `Domain` field + per-domain per-turn verification
   toolsets** — formalise `Domain` on the `Workflow` trait, document
   the in-scope default (`rust-nix`), and define what "per-turn
   verification toolset for domain X" means as a structured object
   (which checks run inside the agent loop, which are advisory, which
   gate the next turn).
2. **(B) Spec issue: decomposition orchestrator** — a workflow whose
   output is a *plan* of sub-domain calls plus their per-step
   per-turn verification, executed by the selector from #204. This is
   the concrete implementation of "Nanna's job within (B) is
   decomposition + domain-specific verification".
3. **(B) Spec issue: zero-verification path for mechanical
   transforms** — a `verification = "none"` workflow class for
   deterministic, well-tested tools (AST transforms, formatters,
   codemods); the selector picks it when the orchestrator has
   decomposed to a single deterministic call. Note this changes only
   per-turn verification; the (A) PR gate is unchanged.
4. **(B) Spec issue: `VerificationLevel` field on `Workflow`** — add
   to the #203 trait + registry, including selector behaviour from
   #204 for security-sensitive tasks. (Was item 1; clarified scope to
   (B).)
5. **(B) Spec issue: light workflow variant for rust+nix** — concrete
   `react-light` implementation that drops blocking container
   isolation from the agent's inner loop and runs `cargo test` only
   advisorily during iteration. The "two-stage promotion" idea from
   §6 becomes redundant once (A)/(B) are separated: every workflow's
   output already passes through (A) before landing on `main`.
   (Was item 2; rescoped against the conflation.)
6. **(B) Eval issue: security eval suite** — prompt-injection /
   exfiltration / out-of-worktree write cases under
   `evals/cases/security/`, runnable by all variants; tests what a
   workflow does inside its loop, not what (A) lets through.
   (Was item 3.)
7. **§4 rewrite (B)** — re-do §4's variant definitions so each
   variant varies (B) only and explicitly holds (A) constant; commit
   the rewrite *before* any pre-registration so the experiment isn't
   measuring an (A)/(B) mixture. (New, replaces the (A)-flavoured
   parts of the original draft.)
8. **(A) Eval issue: coverage-faithfulness eval** — cross-check
   #276-style bypasses end-to-end, not just at PR-time. This is an
   (A) concern: it asks whether agents editing nanna-coder are gaming
   the codecov gate, regardless of what (B) workflow they used.
   (Was item 4; relabelled.)
9. **(B) Eval issue: pre-registration template & decision-rule
   machinery** in `harness::eval::experiment` so A/B comparisons are
   committed before they're run. (Was item 5.)
10. **(B) Runner extension to #207** — accept `VerificationLevel`
    *and* `Domain` as dimensions and emit per-(level, domain)
    matrices in the complementarity report. (Was item 6, extended.)
11. **(B) Reference-systems benchmark** — run Aider and OpenHands
    (sandboxed and unsandboxed) on the same `evals/cases/` subset to
    replace the hand-waved §3 ranking with measurements before any
    nanna-coder workflow ships its `verification` setting. Note that
    the rust+nix-vs-Python domain mismatch must be controlled for
    explicitly: this measures (B) per-turn verification differences,
    not differences in the user-project's CI. (Was item 7.)
