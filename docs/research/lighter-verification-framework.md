# Domain-Specific Verification — Strategy Note for #274

Research note for issue [#274](https://github.com/DominicBurkart/nanna-coder/issues/274).
Follow-up to the ADAS framing in [#196](https://github.com/DominicBurkart/nanna-coder/issues/196).
This note replaces an earlier draft (preserved in PR #277's history)
that pursued the original "lighter verification" framing of #274. The
owner's review on PR #277
([review 4210871987](https://github.com/DominicBurkart/nanna-coder/pull/277#pullrequestreview-4210871987))
redirected: the right axis isn't "how much verification" but
"verification matched to the task's domain". This note is the
restatement of #274 against that frame.

Audience: coding agents and reviewers deciding what to build for the
workflow registry (#203/#204), the complementarity runner (#207), and
the eval suite. The note is opinionated; pick or push back on the
recommended next steps in §6.

## 1. The reframing

Software development is not one domain. nanna-coder's harness is
specialised for rust+nix codebases — its tools, prompts, and per-turn
checks (`cargo build`, `cargo test`, clippy under `-D warnings`, the
rustc LSP, `nix develop` pinning) are calibrated for that domain.
When Nanna is asked to do something else — generate SQL from a natural
language question, translate a spec into prose, run a deterministic
AST transform on a Python file — those primitives don't fit. They
either tell us nothing useful (a `cargo build` against a SQL string)
or they collect signal at a token cost that doesn't bear on the task.

#274 originally framed this as a verification *level* problem: turn
the dial down for tasks where the rust+nix toolset isn't earning its
tokens. That's a workaround. The actual move is to *decompose the
task* and apply the verification primitives that fit each sub-step.
That is domain-specific verification, and it is Nanna's job: figure
out what kind of work a task involves, decompose it into pieces whose
outputs can be validated cleanly, route each piece to a workflow
specialised for its sub-domain, and verify each piece with the
primitives that domain affords.

A single `heavy ↔ light` dial is the right framing in only two cases:

1. **Validation deferred elsewhere.** A check the agent could run
   inside its loop is also run by the user-project's CI (or, when
   Nanna is editing nanna-coder, by nanna-coder's own CI). Skipping
   it inside the loop costs nothing in signal and saves tokens.
2. **Domains that aren't meaningfully decomposable.** If a task
   genuinely can't be split into sub-steps with their own validation
   primitives, per-step verification has nothing to attach to and
   "lighter at the outer level" is the only knob.

Most tasks are not in either bucket. The actionable framing for
everything else is *remove domain-irrelevant verification strategies*
— which is what "domain-specific verification" means in practice and
why the term is more useful than "lighter".

## 2. Scope: per-turn agent behaviour, not nanna-coder's CI

This note is about what Nanna does *inside its agent loop while
solving a user task*: which checks it runs on intermediate outputs,
which signals it consumes, what it spends tokens validating before
committing to the next step.

It is **not** about nanna-coder's own release gates — codecov 100%,
the #275/#276 coverage-bypass guards, AGENTS.md hard rules, the `nix
develop` pinning, the `.github/workflows/` checks. Those gates
protect nanna-coder as a project being built and released. They run
on every PR landed in nanna-coder regardless of which workflow
produced the diff and what user task triggered the work. Nothing
below proposes weakening them.

## 3. Worked example: NL → SQL

A user asks Nanna to generate 500 lines of SQL from a natural-language
question. Two failure modes are possible: the SQL is syntactically
wrong, or it is syntactically valid but expresses something other than
what was asked. A single-pass attempt followed by `cargo build` covers
neither — there is no Rust to build, and the rust+nix primitives have
no purchase on either failure mode.

The domain-specific decomposition has three steps, each with
verification matched to its sub-domain:

**Input filtering.** Reject off-domain or adversarial questions
before they reach a model. A SQL query engine is not the place to ask
about the meaning of life. This is deterministic and cheap; the
filter's contract is its own verification.

**Natural language to a schema-aware intermediate.** A model
translates the question into a structured representation that names
tables and relationships rather than SQL syntax. Verification here is
semantic and necessarily soft: domain alignment (does the
intermediate describe relationships expressible in this schema?),
inversion (does an "agnosticator" model round-trip the intermediate
back to a paraphrase of the input?), internal consistency (formalise
the relationships and check invariants — a LEAN proof can identify
semantic failures and drive iteration). LLM-as-judge with prompts
shaped by these properties gives meaningful signal that no syntactic
check could.

**Intermediate to SQL.** A separate step generates SQL from the
intermediate. Verification here is fully deterministic: parse the
SQL, type-check it against the live schema, dry-run with `EXPLAIN`.

This is *more* per-turn verification than nanna-coder's current loop
applies, not less. But each piece is matched to its sub-domain. The
total token cost is lower than running rust+nix-style verification
that doesn't apply, and the signal is meaningful where the rust+nix
signal would have been noise. Whether to call this "system design" or
"prompt engineering" doesn't matter — under the ADAS framing in #196
it is the same thing: Nanna is formalising and validating its chain
of thought through specialised agents.

## 4. Worked example: mechanical AST transform

The opposite case. An orchestrator decomposes a task to "rename `AAA`
to `BBB` via the AST transform tool". The inner step is a single
call to a deterministic, well-tested tool. The right per-turn
verification is essentially nothing — "I called the tool with the
requested parameters and it returned success". Anything more is
wasted tokens. If a cosmic-ray-class anomaly does corrupt the result,
the orchestrator re-issues the call.

This looks like "lighter verification" but the framing matters: we
are not turning a dial down, we are recognising that a deterministic
tool's contract is the verification. The per-turn loop has nothing
useful to add. (And, to be explicit about §2: if this transform
produces a diff against nanna-coder, the diff still has to clear
nanna-coder's CI before landing.)

## 5. Implications for the registry, selector, and eval suite

### Registry and selector (#203/#204)

Domain-specific verification adds two structured fields to the
`Workflow` trait #203 proposes:

```rust
pub struct Domain(String);
// e.g. "rust-nix", "sql-generation", "natural-language", "mechanical-edit"

pub enum VerificationLevel { None, Light, Medium, Heavy }

pub trait Workflow {
    fn name(&self) -> &str;
    fn domain(&self) -> &Domain;
    fn verification(&self) -> VerificationLevel;
    fn capabilities(&self) -> &[Capability];
    fn suitability_hints(&self) -> &str;
    async fn run(&self, task: Task, ctx: Ctx) -> WorkflowOutcome;
}
```

`Domain` says what the workflow's per-turn primitives are calibrated
for. `VerificationLevel` says how much of those primitives the
workflow runs per turn — `None` for the deterministic-tool case,
`Heavy` for full per-turn checking, the levels in between for cases
where some checks are deferred to CI.

The selector node from #204 picks a workflow whose `Domain` matches
the task and whose `VerificationLevel` is appropriate for the task's
risk profile. A new orchestrator workflow — call it `decompose-and-route`
— produces a *plan* of sub-tasks, each tagged with its own domain,
and routes each sub-task to a specialised workflow. That is the
concrete implementation of "Nanna's job is decomposition +
domain-specific verification".

For security-sensitive tasks (writes outside the worktree, touches
secrets, modifies CI), the selector refuses any
`VerificationLevel < Heavy` regardless of domain.

### Eval suite

The eval suite needs to measure two things the current setup
conflates: (a) whether a workflow's per-turn verification is
appropriate for its declared domain, and (b) whether the registry +
selector pick the right workflow for a given task. Concretely:

- **Per-domain cases.** The current `evals/cases/` is mostly rust+nix
  (`happy-path-*`) and Python via SWE-bench. To measure cross-domain
  performance we need cases in other domains — `evals/cases/sql/`,
  `evals/cases/refactor/`, `evals/cases/mechanical/`.
- **A `(domain, verification)` matrix.** Issue #207's complementarity
  runner emits this directly if extended to take `Domain` as a
  dimension alongside `VerificationLevel`.
- **A security eval suite.** Prompt-injection, exfiltration, and
  out-of-worktree write cases under `evals/cases/security/`,
  runnable by every variant. This catches workflows that bypass
  safety primitives in the name of being domain-specific.
- **Pre-registered decision rules.** Variants and their pass/fail
  thresholds committed before the experiment runs, so post-hoc
  justification of regressions is syntactically blocked. The
  `.github/workflows/eval.yml` default model
  (`workflow_dispatch.inputs.model.default`, currently `qwen3:0.6b`)
  must be read at commit time rather than encoded in the experiment
  record.

A narrower experiment is also worth running in parallel: a
rust+nix-only A/B test of `react-default` (current `Heavy` loop)
against a `react-light` variant that drops the blocking container and
treats coverage as advisory inside the loop. This measures whether
deferring redundant rust+nix checks to CI changes outcomes within the
rust+nix domain. It is calibration, not the main result.

## 6. Reference systems

For external calibration, three points on the per-turn verification
axis from publicly available systems. None of them decompose by
sub-domain in the way §1 proposes; the comparisons are useful for the
narrower "deferred-to-CI" question, not the broader "decompose +
domain-specific" question.

**Aider.** Thin CLI against an existing checkout; per-turn
verification is the user's pre-commit hooks plus an optional
`--test-cmd`. No sandbox, no enforced coverage. The human is the
gate. Aider's published SWE-bench numbers are competitive
(historically top-tier on the Verified leaderboard, including
multi-file edits); per-domain breakdowns by orchestration depth are
not, to our knowledge, published — treat any
orchestration-vs-edit-locality split as a hypothesis to test rather
than a finding.

**OpenHands without sandbox.** Ships with a runtime sandbox by
default but exposes a "local runtime" mode for low-friction
iteration. Disabling the sandbox saves wall time on small edit tasks;
published comparisons on full SWE-bench-verified are scarce. Treat
any performance ranking as a hypothesis.

**Raw tool-use harnesses.** A category, not a project: many in-house
agentic harnesses run with a single `bash` tool, a read/write tool,
and CI that's "format + unit tests, advisory coverage". They tend to
dominate on greenfield generation and underperform on
regression-heavy maintenance.

## 7. Recommended next steps (do not file yet)

1. **Spec issue: `Domain` field on `Workflow`.** Formalise the field
   on the #203 trait, document the in-scope default (`rust-nix`),
   and define what "per-turn verification toolset for domain X"
   means as a structured object: which checks fire, which are
   advisory, which gate the next turn.
2. **Spec issue: `VerificationLevel` field on `Workflow`.** Including
   the `None` case for deterministic-tool workflows and the selector
   behaviour from #204 that refuses sub-`Heavy` for security-sensitive
   tasks.
3. **Spec issue: `decompose-and-route` orchestrator.** A workflow
   that produces a plan of sub-domain calls plus their per-step
   verification, executed by the selector. This is the concrete
   implementation of the §1 reframing.
4. **Spec issue: `react-light` workflow for rust+nix.** A concrete
   light variant in the rust+nix domain (no blocking container in
   the inner loop, advisory coverage), as the in-domain calibration
   point for the eval matrix.
5. **Eval issue: per-domain case suite.** Add cases under
   `evals/cases/sql/`, `evals/cases/refactor/`,
   `evals/cases/mechanical/` so the eval suite can measure
   cross-domain transfer rather than just rust+nix performance.
6. **Eval issue: security suite.** Prompt-injection, exfiltration,
   and out-of-worktree write cases under `evals/cases/security/`,
   runnable by every variant.
7. **Eval issue: reference-systems benchmark.** Run Aider, OpenHands
   (sandboxed and unsandboxed), and a `decompose-and-route` Nanna
   variant on the same `evals/cases/` subset, with the
   rust+nix-vs-other domain split controlled for explicitly. This
   replaces the hand-waved §6 ranking with measurements before any
   nanna-coder workflow ships its `verification` setting.
8. **Runner extension to #207.** Accept `(Domain, VerificationLevel)`
   as a joint dimension and emit per-cell matrices in the
   complementarity report.
9. **Eval issue: pre-registration template & decision-rule
   machinery** in `harness::eval::experiment` so A/B comparisons are
   committed before they run.
