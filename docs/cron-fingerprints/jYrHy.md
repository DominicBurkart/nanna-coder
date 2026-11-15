# Cron fingerprint: jYrHy

- Job: stateless LIFO issue-owner CRON (claude-opus-4-7 one-shot)
- Run date: 2026-05-24
- Oldest-issue-id (LIFO anchor across managed repos): `one_track#2`
- Same-trigger rule: per author-stipulation (1), this run is a **no-op** for any
  issue whose canonical PR is already at an unchanged head SHA. Disposition is
  recorded here so duplicates can be detected/closed on sight.

## Per-issue disposition (nanna-coder)

| issue | title | canonical PR | head SHA | disposition |
|------:|-------|------------:|----------|-------------|
| #5  | monitoring: CI health monitoring &amp; performance metrics | **#247** | 8a589694 | NO-OP — defer to #247 (mergeable_state=behind; rebase wanted, but #247 is blocked on #6/PR#243 composite-action consolidation; planner job will not rebase another planner's PR). Coverage follow-ups #412 and #388/#354 are downstream. |
| #10 | docs: comprehensive CI maintenance/troubleshooting documentation | **#254** | d9ccf860 | NO-OP — defer to #254 (mergeable_state=behind; ci.yml docs-check wiring requires `workflows` token scope which no cron agent holds; janitor must apply the 13-line patch directly). Doc-trim follow-up #306/#408 are downstream. |
| #20 | Entity Management (epic) | — | — | NO-OP — both acceptance criteria satisfied (ARCHITECTURE.md present with both mermaid diagrams; six sub-issues #22/#23/#24/#25/#26/#27 created). Triage comment dated Apr 22 recommends maintainer-close. Planner jobs do not promote/close (clause 3) — flagged for janitor. |
| #23 | Entity Type: AST &amp; Filesystem Entities | **#258** | 7c2b7ed9 | NO-OP — defer to #258 (mergeable_state=dirty; Rust slice via `syn 2.0`; per-language follow-ups Python/JS/TS/Java/YAML/TOML/JSON/CSV/Dockerfile/Nix/Make/CMake/sh deferred as explicit slices). Coverage follow-up #409 is downstream. |
| #24 | Entity Type: Testing &amp; Analysis Entities | **#267** | f229f365 | NO-OP — defer to #267 (mergeable_state=behind; partial close — nightly→stable nextest + 7 review threads resolved; deferred: Coverage, SecurityAudit, TrendAnalysis, flaky detector, pre-commit hook, FileEntity cross-ref proptest serde roundtrip). **Hard-depends on #23 / PR #258** for `LintLocation.file → FileEntity.id` cross-ref — blocked until #258 lands. Coverage follow-up #411 is downstream. |
| #39 | Migrate from Ollama to vLLM | **#140** | 79c94397 | NO-OP — defer to #140 (mergeable_state=dirty; the OpenAI-compatible gateway provider that is the de-concretization seam for vLLM). Janitor plan (per #39 thread): land #140 → ModelJudge blanket impl refactor → de-concretize harness call sites → introduce vLLM `ContainerKind` → CI matrix → flip default → delete Ollama. Phase-0 coverage follow-up #410 is downstream. |

## Epics with no canonical PR (no-op + decomposition flag)

These four issues opened 2026-03-01 (`#60`/`#61`/`#62`/`#63`) plus `#84`
(2026-03-22) describe multi-week feature work that **a single cron pass cannot
deliver end-to-end**. None has a canonical PR. Planner job records the
recommendation; janitor session(s) should break them down before assigning
ownership.

| issue | title | recommendation |
|------:|-------|---------------|
| #60   | Expose Nanna via MCP | Decompose: (a) `nanna-mcp` crate skeleton with `serve` subcommand, (b) `task.submit`/`task.poll`/`task.report` tool schemas, (c) Claude Code MCP install quickstart + smoke test. Each (a)/(b)/(c) is a single cron-tractable PR. **Hard-depends** on #61, #62, #63 — the user-visible MCP wrapper sits on top of those. Track as epic. |
| #61   | MCP server infrastructure: protocol, transport, tool schema | Decompose: (a) `rmcp`/`mcp-sdk` choice + Cargo dep + minimal stdio transport, (b) tool registration via `serde`/schemars, (c) connection lifecycle + error envelope. (a) is cron-tractable; (b)+(c) are 1–2 PRs each. |
| #62   | Task lifecycle management: data model, state machine, dispatch | Decompose: (a) `TaskId` newtype + `TaskState` enum + persistence shape (no executor), (b) state-transition `kani` proofs (cf. open kani umbrella issues #145–#155), (c) dispatcher worker. (a) is cron-tractable. |
| #63   | Shared model container lifecycle management | Decompose: (a) container-handle abstraction (already partially in `harness::container`), (b) ref-counting / Drop semantics across harnesses, (c) startup health-check protocol. Each cron-tractable. |
| #84   | agentic eval suite | Decompose: (a) `harness::eval::swebench` materializer (PR #392 / `harness/src/eval/swebench` already exists at HEAD — read first), (b) SWE-bench runner that produces a `report.md` + mermaid graph, (c) GitHub workflow that posts the report as a PR comment on manual dispatch. (a) is partially done; (b)+(c) are independent cron-tractable PRs. **Note**: #92 (resource utilization) is downstream of (b). |

## Promote-to-human (for janitor)

Per author-stipulation (3), **planner jobs never promote**. This PR is
intentionally **not** labeled `ready-for-review`. Janitor actions wanted:

1. Promote (label `ready-for-review`) and merge the canonical PRs in the
   order: #258 → #267 → #247 → #254 → #140 → #142 (cross-repo, velib-mcp).
2. Close #20 with state_reason=completed (triage comment Apr 22 confirms ACs met).
3. Apply the 13-line ci.yml docs-check wiring on top of #254 (requires
   `workflows` token scope).
4. Apply the 1-line `--no-gpg-sign` to `.github/workflows/badges.yaml`
   (cross-repo, marigold — same `workflows`-scope blocker; see marigold cron
   fingerprint `jYrHy.md`).
5. Close duplicate planner PRs: #354 / #388 (superseded by #407 coverage scope);
   #189 (already-red CI duplicates of master state).
6. Open janitor decomposition issues for epics #60 / #61 / #62 / #63 / #84
   per the recommended slices above.

## Fingerprint

```
job-id: jYrHy
oldest-issue-id: one_track-2
this-cron: lifo-cron-2026-05-24
prior-cron-of-same-trigger: lifo-cron-2026-05-23 (WLfKh) — produced canonical PRs #215-#222 in one_track
nanna-coder issues covered: 5, 10, 20, 23, 24, 39 + epic flags 60, 61, 62, 63, 84
```

## Tags

- `agent-job:jYrHy`
- `oldest:one_track-2`
