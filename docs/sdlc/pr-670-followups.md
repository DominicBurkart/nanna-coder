# PR #670 follow-ups (deploy gating)

PR #670 (`feat/sdlc-outer-base`) merged to main at 0e4f5d7 on 2026-10-04 before
its re-review was complete. A post-merge review (on #670) did not accept the
deploy gating as enforced. Until the items below land, `nanna deploy plan`
output and docs must describe the plan as advisory. #799 does that.

## What is on main

- Case-insensitive production gate: `Production` requires window and health
  preconditions the same as `production` (#776, fix present, issue open).
- Plan-time validation in `plan_with_score` (#775, fix present, issue open).
- `nanna deploy plan` labels its output and JSON as advisory (#799, merged).

## Not enforced on main

| Item | Issue | State | Why it matters |
|---|---|---|---|
| Rollout executor evaluates window, health and lease preconditions | #650 | open | Declared preconditions are never evaluated. |
| Lease enforcement; nothing acquires the deploy lease | #737 | open | Two deploys can collide on one target. |
| `windows.toml` lives in the target repo, which agents can write | #734 | open | An agent can loosen its own deploy windows; `deploy plan` reads that file. |
| Gating template read from the target repo (agent-editable) | #738 | open | An agent can edit its own deploy gates; lease names use the image basename. |
| `deploy.toml` schema ratification | #649 | closed | Schema was not ratified before the issue closed; confirm before relying on it. |

## Close-out

Close #775 and #776 once a re-review of 0e4f5d7 accepts their fixes.
Close #734, #737 and #738 only with code that enforces them. Close #650 when
the executor evaluates every precondition on the production path.
