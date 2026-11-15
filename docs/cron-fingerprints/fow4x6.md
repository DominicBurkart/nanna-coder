# CRON fingerprint `fow4x6` — nanna-coder slice

- job-id: `fow4x6`
- oldest-issue-id: `one_track#2`
- parent-fingerprint: #437 (`blxz9d`)
- run at: 2026-06-10

## Trigger

Stateless LIFO issue-owner CRON, oldest-15 `author:DominicBurkart` open issues across
managed repos.
This is the **nanna-coder** slice — 6 issues (#5, #10, #20 epic, #23, #24, #39).
Sibling slices for the same trigger: `one_track` (#2 #3 #4 #5 #6 #7 #8 #9), `velib-mcp` (#9).

## Fingerprint table — byte-identical no-op vs `blxz9d`

| issue | canonical PR              | head SHA @ `blxz9d`     | head SHA @ `fow4x6`     | Δ |
|------:|---------------------------|-------------------------|-------------------------|---|
| #5    | #247                      | `8a589694`              | `8a589694`              | — |
| #10   | #254                      | `d9ccf860`              | `d9ccf860`              | — |
| #20   | EPIC (no PR)              | n/a                     | n/a                     | — |
| #23   | #258                      | `7c2b7ed9`              | `7c2b7ed9`              | — |
| #24   | #267                      | `f229f365`              | `f229f365`              | — |
| #39   | #140 (code) + #336 (plan) | `79c94397` / `e270a211` | `79c94397` / `e270a211` | — |

Base `main` unchanged at `c71e6114` since `blxz9d`. **None of the canonical PR heads moved.**

True byte-identical no-op re-run. Extends the chain
`blxz9d` ← `FdH5f` ← `XGHi2` ← `NUZKb` ← `ILrvv` ← `B0JHr` ← `eTYOX` ← `QkcXO`
← `VFjIU` ← `TzdJX` ← `HTzrH` ← `jYrHy` ← `dRcjT` ← `fyhme` ← `myPy6` ← `pZdbm`
← `MvlE2` ← `dRwfn` ← `EFJnO` ← `X16QR` ← `xR5Hn`.

## Per-issue disposition

| issue | disposition |
|------:|-------------|
| #5  | **NO-OP** — defer to #247 (partial; blocked on #243 + `workflows` token scope) |
| #10 | **NO-OP** — defer to #254 (blocked on `workflows` token scope) |
| #20 | **NO-OP** — epic; defer to leaves #23, #24 |
| #23 | **NO-OP** — defer to #258 (`dirty`; codecov diff 92.70%) |
| #24 | **NO-OP** — defer to #267 (`behind`; codecov diff 80.88%; blocked on #23) |
| #39 | **NO-OP** — defer to #140 (phase-1 OpenAI-compat gateway) + #336 (plan) |

## Owner-assign clause

Not triggered — every in-scope issue has prior work and is already assigned to
`DominicBurkart`. No assignment mutations performed.

## Why this CRON ships no code

1. Canonical PRs exist for every code-bearing in-scope issue (#247, #254, #258, #267,
   #140); pushing parallel implementations would create duplicate-PR sprawl (rule 2).
2. #247 / #254 / #258 / #267 / #140 are partial-progress PRs authored by other agent
   jobs; this cron does not push to branches it does not author.
3. #254's blocker is a token-permission problem (`workflows` scope) — this cron's
   token also lacks that scope.
4. #258 (`dirty`) and #140 (`dirty`) need rebase by their original authors before any
   code can be layered on top.
5. #39's prompt premise interacts with active design churn on #140 (OpenAI-compat
   gateway). Human-in-the-loop should confirm intent before phase 2.

## Janitor carry-over

- Rebase #247, #254, #258, #267, #140 against current `main` (`c71e6114`); #258 and
  #140 need conflict resolution (`dirty`).
- #254's `docs-check` wiring needs a maintainer token with `workflows` scope.
- #267's deferred follow-ups for #24 (`CoverageResult`, `SecurityAudit`,
  `TrendAnalysis`, flaky-test detector, pre-commit hook, `LintLocation.file` →
  `FileEntity.id` cross-ref [blocked on #23], proptest serde roundtrip) remain open
  as explicit follow-up PRs.
- Close superseded `affectionate-hawking-*` fingerprint PRs: #437 (`blxz9d`),
  #435 (`FdH5f`), #430 (`XGHi2`), and prior chain entries on sight. Keep #336 for
  its plan content but supersede its fingerprint role.

## Promote-to-human

Per CRON contract clause (3) — **planner jobs never promote**. This PR is intentionally
**NOT** labeled `ready-for-review`. The janitor decides promotion.

## Tags

- `agent-job:fow4x6`
- `oldest:one_track-2`
