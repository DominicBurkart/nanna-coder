# cron-FQlew — fingerprint manifest (nanna-coder slice)

```
job-id:           FQlew
oldest-issue-id:  one_track#2
fingerprint-date: 2026-05-13
prior-crons:      xM9Bl (#247), qsAIJ (#254), QO4Td (#267),
                  fyhme (PR #336), xR5Hn (PR #379)
```

## Run scope

Cross-repo run covering the 15 oldest `author:DominicBurkart` open
issues across managed repos. This file is the **nanna-coder** slice
(issues #5, #10, #20, #23, #24, #39). Companion manifests live in
`one_track` and `velib-mcp` on branches `claude/<name>-FQlew`.

## Per-issue disposition

| issue | canonical PR     | head sha   | mergeable | disposition |
|------:|-----------------:|------------|-----------|-------------|
| #5    | **#247**         | `8a589694` | behind    | NO-OP — defer to #247 (CI metrics composite action) |
| #10   | **#254**         | `d9ccf860` | behind    | NO-OP — defer to #254 (`docs/ci/` + link/coverage checks) |
| #20   | EPIC             | n/a        | n/a       | NO-OP — epic, defer to children #23, #24 |
| #23   | **#258**         | `7c2b7ed9` | behind    | NO-OP — defer to #258 (partial; RustAstEntity first slice) |
| #24   | **#267**         | `f229f365` | behind    | NO-OP — defer to #267 (partial; stable-nextest + 7 OWNER threads) |
| #39   | plan in **#336** | `e270a211` | behind    | NO-OP — plan-only PR, defer to #336 |

## Stale head check (vs prior cron xR5Hn, 2026-05-10)

| PR  | xR5Hn recorded sha | current head sha (2026-05-13) | drift |
|----:|--------------------|-------------------------------|-------|
| #247 | `8a589694` | `8a589694` | none  |
| #254 | `d9ccf860` | `d9ccf860` | none  |
| #258 | `7c2b7ed9` | `7c2b7ed9` | none  |
| #267 | `f229f365` | `f229f365` | none  |
| #336 | `e270a211` | `e270a211` | none  |

No drift on any canonical PR head since xR5Hn (3 days ago). This is
exactly the "same trigger → same no-op" branch of rule (1).

Base `main` has advanced (`72c5e1b9` → `dde3f940`) since xR5Hn, which
pushes the canonical PRs further `behind` but does not affect their
content. Rebase remains the immediate gate.

## Flagged for follow-up (carried from prior crons)

- **PR #258** (#23 partial) — codecov diff at 92.70% < 100% target as of
  fyhme. Uncovered paths documented in `fyhme.md`.
- **PR #267** (#24 partial) — codecov diff at 80.88% < 100% target as of
  fyhme. Same re-check.
- All four code-bearing PRs (#247, #254, #258, #267) are
  `mergeable_state=behind` against `dde3f940`. Rebase / squash-merge
  against current `main` is the immediate gate.
- PR #254 has a known blocker: the `docs-check` CI wire-up needs a
  workflows-scoped token, which the cron agent did not have. A janitor
  with workflows scope (or a human) should land the 13-line patch
  documented in PR #254's body.

## #39 (Ollama → vLLM) — still a plan, no code

PR #336 (cron-fyhme) is the plan-only artifact for #39. The five-phase
plan (OpenAI-compat provider → vLLM container → harness wiring →
default-flip → ROCm variant) and the headline risks (model-format swap,
tool-call parity on Qwen3, vLLM CPU cold-start, in-flight conflicts
with #335 / #290) all still apply.

Per fyhme's analysis the issue prompt is factually incorrect about
PR #140 — the human should confirm intent before phase 1 begins.

## Promote-to-human

Per CRON contract clause (3) — **planner jobs never promote**. This PR
is intentionally **NOT** labeled `ready-for-review`. The janitor job
decides promotion based on the canonical PRs, not this manifest.

## Duplicate-detection guidance for janitor

This manifest PR carries the same job-id (`FQlew`) and oldest-issue-id
(`one_track-2`) labels as the companion manifests in `one_track` and
`velib-mcp`. Prior-cron manifest PRs #379 (`xR5Hn`) and #336 (`fyhme`)
cover the same issues with identical dispositions; on sight, all three
can be closed by the janitor once the canonical PRs are promoted /
rebased.

## Tags

- `agent-job:FQlew`
- `oldest:one_track-2`
