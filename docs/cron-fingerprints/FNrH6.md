# CRON job FNrH6 — fingerprint and dispositions

- job-id: FNrH6
- oldest-issue-id: `one_track#2` (cross-repo run)
- branch: `claude/compassionate-lamport-FNrH6`
- base: `main`
- scope: #5, #10, #20 (epic), #23, #24, #39
- fingerprint date (UTC): 2026-05-09
- prior cron: fyhme (PR #336)

## Per-issue disposition

| issue | title | canonical PR | head sha | action |
|------:|-------|-------------:|----------|--------|
| #5 | monitoring: CI health monitoring + perf metrics | #247 | `8a58969` | NO-OP — defer (gated on PR #243 + workflows-token escalation) |
| #10 | docs: CI maintenance + troubleshooting documentation | #254 | `d9ccf860` | NO-OP — defer (gated on workflows-token escalation; commit `63e3e6d` unpushed) |
| #20 | Entity Management (epic) | n/a | n/a | EPIC — open subs #23, #24; defer to leaves |
| #23 | Entity Type: AST & Filesystem Entities | #258 | `7c2b7ed` | NO-OP — defer (no local cargo env to verify codecov 100% gate) |
| #24 | Entity Type: Testing & Analysis Entities | #267 | `f229f36` | NO-OP — defer (codecov 80.88% < 100%; no local cargo env to verify) |
| #39 | Migrate from Ollama to vLLM | (plan in #336) | n/a | NO-OP — gated on PR #140 (OpenAI-compat gateway) landing |

## Cross-repo escalation: workflows-token scope

PRs #247 (#5) and #254 (#10) are both blocked on the same root cause: the agent token used
by prior crons lacks the GitHub `workflows` scope, so neither PR can push the workflow-edit
commit that wires its scaffolding into CI.

- **PR #247 (#5)** — wiring blocked until **PR #243** (composite-action consolidation)
  lands; the wiring commit will then need a human with `workflows`-scope token to push.
- **PR #254 (#10)** — local commit `63e3e6d` (`docs-check` job + `all-checks.needs` add)
  was prepared but never pushed. Same blocker.

**Action for janitor**: a human with a `workflows`-scope token cherry-picks `63e3e6d` from
PR #254 and re-applies the equivalent wiring on PR #247 once #243 merges. Without this, the
cron will record the same NO-OP indefinitely.

This pattern recurs across `marigold/.github/workflows/badges.yaml:47` (`git commit -m
"update badges"` missing `--no-gpg-sign`); see the marigold cron-FNrH6 manifest for that
escalation.

## Per-issue notes

### #23 — extends PR #258 (RustAstEntity)

PR #258 ships the Rust slice (syn-based RustAstEntity + query helper + criterion bench),
draft, behind main, codecov 92.7% deferred. Sister entity types #22 (git), #25 (env), #26
(context) are merged exemplars; #27 deferred.

Outstanding follow-ups (mirroring #258 + #260 first-slice pattern):

- `entities/ast/python.rs` (tree-sitter-python) using `AstSummary { functions, classes,
  imports }` shape
- `entities/text.rs` `TextFileEntity { path, line_count, char_count, lines, encoding }`
  per issue body
- `entities/binary.rs` `BinaryFileEntity { path, size, mime_type, base64_content,
  metadata }` per issue body
- `entities/fallback.rs` dispatcher (`entity_for_path`) returning Rust → Python → Text →
  Binary
- `proptest!` UTF-8 line-counting roundtrip in `text.rs`
- Criterion bench `bench_parse_python_realistic`, `bench_text_file_index_10k_lines`
- New deps: `tree-sitter`, `tree-sitter-python`, `mime_guess`, `base64`

Recommended split into two follow-up PRs to keep the codecov diff-gate tractable: (1)
Python AST + dispatcher; (2) TextFile + BinaryFile entities.

Required pre-push gate (must run locally before any push):

```sh
nix develop --command bash -c '
  cargo test -p harness && \
  cargo clippy --all-targets -- -D warnings && \
  cargo fmt --check && \
  cargo llvm-cov --fail-under-lines 100 --no-report
'
```

This cron has no cargo / nix environment; deferring to a janitor or follow-up cron with
local toolchain.

### #24 — iterates PR #267 (test-entities)

PR #267 (1161 LOC across 11 files) is mostly complete. CI all green except
`codecov/patch=80.88%` (target 100%). The 7 review threads cited in the title are now
resolved (`totalCount=0`). `mergeable_state=behind`.

Gating action: rebase onto current main + add tests against codecov-reported uncovered
ranges (NOT against guessed function names). Required local pre-push:

```sh
cargo llvm-cov --html --open  # find uncovered ranges by file
# write tests targeting those ranges
cargo llvm-cov --fail-under-lines 100 --no-report
cargo test -p harness && cargo clippy -- -D warnings && cargo fmt --check
```

Probable uncovered surface (to verify with llvm-cov, not guess): error paths in
`executor.rs` (process spawn failure, non-UTF-8 stdout) and `correlation.rs` (Validates edge
construction failures). The "~80 LOC of tests" estimate from earlier planning is likely
2-3x low.

This cron has no cargo env; defers to janitor or follow-up cron.

### #39 — vLLM migration is dependency-blocked

Real dependency chain: **PR #140** (OpenAI-compat gateway) → ModelJudge blanket impl →
harness de-concretize OllamaProvider → vLLM container → CI matrix → default-flip → Ollama
removal.

The "PR-A…of 9" series (#368-#371) is the multi-track eval epic, NOT the vLLM seam. PR #367
referenced as parent of #371 is an issue, not a PR. Prior plan in PR #336 remains current;
this run does not duplicate it.

Janitor: triage **PR #140** to mergeable green is the single unblock for #39 and the entire
6-stage downstream chain.

## Rebase coordination (#23 vs #24)

Disjoint paths (`entities/ast/` vs `entities/test/` + `agent/eval.rs`). No shared
`Cargo.toml` conflicts unless #23's new deps (`tree-sitter` etc.) collide with #24's
lockfile. Suggested order: **#24 first** (smaller, closer to merge), then **#23** rebases
onto post-#24 main. Both must rebase onto a moving main.

## Phase 2 — Self-review

- All 6 in-scope issues are NO-OPs against existing canonical PRs/plans.
- Two recurring blockers surfaced: (a) workflows-token scope (#5, #10), (b) PR #140 not yet
  landed (#39 + entire vLLM chain).
- #23 and #24 are deferred not because the work is unclear but because this cron has no
  local cargo / nix environment to verify the codecov-100% gate before pushing untested
  test code.

## Phase 3 — Implementation

This branch contains only this fingerprint document.

## Promotion / janitor

Per CRON contract clause (3): planner jobs never promote. This PR is intentionally NOT
labeled `ready-for-review`.

## Idempotency / fingerprint

Re-running this CRON produces the same no-op.

## Tags

- job-id: FNrH6
- oldest-issue-id: `one_track#2`
