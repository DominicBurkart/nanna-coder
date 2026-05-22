# CRON fingerprint manifest — job `dRcjT`

- job-id: `dRcjT`
- oldest-issue-id: `one_track#2`
- run date: 2026-05-22
- repo slice: **nanna-coder** — 6 of the oldest 15 `author:DominicBurkart` open issues

Stateless LIFO issue-owner CRON (see https://dominic.computer/blog/2026/routines).
Per fingerprint rule (1), every in-scope issue is fingerprinted to its canonical
open PR (`issue → open-PR → head-SHA`) **before** any work is spawned, so an
unchanged trigger produces an unchanged no-op. Every in-scope issue is already
addressed by a canonical PR (or, for #39, a plan-only PR) — this branch
**defers**. The only artifact is this manifest; no source is touched.

## Per-issue disposition

| issue | title | canonical PR | head SHA | mergeable | disposition |
|------:|-------|-------------:|----------|-----------|-------------|
| #5  | CI health monitoring & perf metrics | #247 | `8a589694` | behind | NO-OP — defer to #247 (partial) |
| #10 | CI maintenance & troubleshooting docs | #254 | `d9ccf860` | behind | NO-OP — defer to #254 (partial; blocker below) |
| #20 | Entity Management | EPIC | n/a | n/a | NO-OP — epic; defer to leaves #23, #24 |
| #23 | AST & filesystem entities | #258 | `7c2b7ed9` | dirty | NO-OP — defer to #258 (partial) |
| #24 | testing & analysis entities | #267 | `f229f365` | behind | NO-OP — defer to #267 (partial) |
| #39 | migrate Ollama → vLLM | #336 (plan only) | `e270a211` | behind | NO-OP — plan-only PR; defer to #336 |

All six issues are `author:DominicBurkart` and already assigned to
`DominicBurkart` with prior work — the "assign owner when unassigned and
unstarted" rule does not fire for any of them.

## Drift check vs prior cron `pZdbm` (PR #401, 2026-05-19)

- Base `main` unchanged @ `c71e6114`.
- All five code/plan-bearing PR heads byte-identical to the `pZdbm` snapshot:
  #247 `8a589694`, #254 `d9ccf860`, #258 `7c2b7ed9`, #267 `f229f365`,
  #336 `e270a211`.
- Zero in-scope drift — a true no-op re-run.

## Gaps carried forward (for the janitor, not actioned by this planner job)

These are recorded here rather than re-posted as comments on the canonical PRs —
prior cron runs already documented them on-thread, and re-commenting would only
add noise.

- **#5 / PR #247** ships a minimum-viable slice (a `ci-metrics` composite action)
  and explicitly defers the aggregator job and external/public dashboard to
  follow-ups. #5 is *partially*, not fully, delivered. Completing it is a
  scoping/promotion call for the janitor; a planner job does not open competing
  PRs. PR also needs a rebase (`behind`).
- **#10 / PR #254** is blocked: wiring the `docs-check` CI job needs a token with
  `workflows` scope, which the agent token lacks. This cron's token also lacks
  it — the blocker carries unchanged. The 13-line wiring patch is posted on #254.
  PR also needs a rebase (`behind`).
- **#23 / PR #258** is a first-language slice (Rust-only `RustAstEntity`); the
  multi-language rollout, binary/text fallbacks, modify API, and relationship
  graph remain as follow-ups. `mergeable_state=dirty` — needs conflict
  resolution. Prior runs flagged a codecov diff gap (~92.70%).
- **#24 / PR #267** delivers the nextest/stable-toolchain slice; coverage/audit/
  trend/flaky-detection/validation-hook remain follow-ups, and the
  `LintLocation.file → FileEntity` cross-ref is itself blocked on #23. Prior runs
  flagged a codecov diff gap (~80.88%). PR needs a rebase (`behind`).
- **#39 / PR #336** is plan-only (no code). The issue prompt's premise that an
  `OpenAiProvider` already exists via a merged PR #140 is factually incorrect — a
  code search disproves it. A human should confirm intent before phase 1 of the
  vLLM migration begins. The cross-cutting Rust+Nix+CI+model-format surface also
  argues against a single mega-PR.

## Duplicate-PR carry-over (rule 2)

Close on sight as superseded by this snapshot, detectable via the
`agent-job:*` + `oldest:one_track-2` label pair on `affectionate-hawking-*`
branches:

- #404 (`myPy6`), #401 (`pZdbm`), #399 (`MvlE2`), #397 (`dRwfn`), #395 (`EFJnO`),
  #393 (`X16QR`), #379 (`xR5Hn`) — prior `affectionate-hawking-*` fingerprint PRs.
- #336 (`fyhme`) — keep the #39 **plan** content, but its fingerprint role is
  superseded by this manifest.

## Promote-to-human

Per CRON contract clause (3) — **planner jobs never promote**. This PR is
intentionally **not** labeled `ready-for-review`. The janitor job decides
promotion.

## Tags

- `agent-job:dRcjT`
- `oldest:one_track-2`
