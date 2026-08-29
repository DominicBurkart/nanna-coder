# cron-xR5Hn — fingerprint manifest (nanna-coder slice)

```
job-id:           xR5Hn
oldest-issue-id:  one_track#2
fingerprint-date: 2026-05-10
prior-crons:      xM9Bl (#247), qsAIJ (#254), QO4Td (#267), fyhme (#336)
```

## Run scope

Cross-repo run covering the 15 oldest `author:DominicBurkart` open
issues across managed repos. This file is the **nanna-coder** slice.
Companion manifests live in `one_track`, `velib-mcp`, and `marigold`
on branches `claude/<name>-xR5Hn`.

## Per-issue disposition

| issue | canonical PR     | head sha     | mergeable | disposition |
|------:|-----------------:|--------------|-----------|-------------|
| #5    | **#247**         | `8a589694`   | behind    | NO-OP — defer to #247 (CI metrics composite action) |
| #10   | **#254**         | `d9ccf860`   | behind    | NO-OP — defer to #254 (`docs/ci/` + link/coverage checks) |
| #20   | EPIC             | n/a          | n/a       | NO-OP — epic, defer to children #23, #24 |
| #23   | **#258**         | `7c2b7ed9`   | behind    | NO-OP — defer to #258 (RustAstEntity first slice, partial) |
| #24   | **#267**         | `f229f365`   | behind    | NO-OP — defer to #267 (stable nextest + 7 review threads, partial) |
| #39   | plan in **#336** | `e270a211`   | behind    | NO-OP — plan-only PR, defer to #336 |

## Stale head check (vs prior cron fyhme)

| PR | fyhme recorded sha | current head sha | drift |
|---:|--------------------|------------------|-------|
| #247 | `8a58969` | `8a589694` | none (display truncation) |
| #254 | `d21eebc` | `d9ccf860` | **moved** — branch has advanced; rebase / new commits since 2026-05-01 |
| #258 | `7c2b7ed` | `7c2b7ed9` | none (display truncation) |
| #267 | `f229f36` | `f229f365` | none (display truncation) |
| #336 | (this PR) | `e270a211` | n/a |

PR #254 head advanced since cron-fyhme. The fyhme blocker-note
("workflows token scope missing for the `docs-check` CI wire-up")
likely still applies — verify in the latest commit before promotion.

## Flagged for follow-up (carried from cron-fyhme)

- **PR #258** (#23 partial) — codecov diff at 92.70% < 100% target as of
  fyhme. Re-check; if still under, the uncovered paths are documented
  in the cron-fyhme manifest.
- **PR #267** (#24 partial) — codecov diff at 80.88% < 100% target as of
  fyhme. Same re-check.
- All four code-bearing PRs (#247, #254, #258, #267) are
  `mergeable_state=behind`. A rebase against current `main` is the
  immediate gate to merge.

## #39 (Ollama → vLLM) — still a plan, no code

PR #336 (cron-fyhme) is the plan-only artifact for #39. The five-phase
plan (OpenAI-compat provider → vLLM container → harness wiring →
default-flip → ROCm variant) and the headline risks (model-format swap,
tool-call parity on Qwen3, vLLM CPU cold-start, in-flight conflicts
with #335 / #290) all still apply.

Per fyhme's analysis the issue prompt is factually incorrect about
PR #140 — the human should confirm intent before phase 1 begins.

## Promote-to-human

Per CRON contract clause (3) — **planner jobs never promote**. This
PR is intentionally **NOT** labeled `ready-for-review`. The janitor
job decides promotion based on the canonical PRs, not this manifest.

## Tags

- `agent-job:xR5Hn`
- `oldest:one_track-2`
