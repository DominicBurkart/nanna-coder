# CI Architecture

This document describes the architecture of Nanna Coder's CI/CD system —
what each workflow does, how they fit together, and the design decisions
behind the split.

> **Related**: [`docs/ci-cd-pipeline.md`](../ci-cd-pipeline.md) has the
> long-form pipeline tour; this file is the maintainer-oriented map.

## Workflow Inventory

| File | Trigger | Purpose |
|------|---------|---------|
| `ci.yml` | push to `main`/`develop`, PR to `main`, releases | Primary gate. Runs the full test/build/container matrix and the `all-checks` aggregator. |
| `ci-integration.yml` | PRs touching CI infra, weekly cron, manual | Self-tests for the CI itself (container loading, cold-cache fallback, negative test). |
| `ci-metrics.yml` | every workflow_run completion on `ci.yml` | Aggregates build durations and cache outcomes into the workflow summary. Tracks issue #5. |
| `codecov-guard.yml` | PR/push touching `codecov.yml` | Rejects silent relaxations of coverage targets (lowered `target:`, new `ignore:` entries, numeric → `auto`). |
| `cache-warming.yml` | push to `main` touching lock files; manual | Pre-populates Cachix so PR builds start warm. |
| `eval.yml` | manual (`workflow_dispatch`) | Runs the LLM eval suite against a chosen model. |
| `install-test.yml` / `install-nightly.yml` | PR / nightly | Validates the install scripts on a clean runner. |

## Topology

```
                       ┌──────────────────────┐
   push / PR ─────────▶│      ci.yml          │── all-checks ─▶ green
                       │  (test/build/cont.)  │
                       └──────────┬───────────┘
                                  │ workflow_run
                                  ▼
                       ┌──────────────────────┐
                       │   ci-metrics.yml     │── step summary
                       └──────────────────────┘

   PR touching        ┌──────────────────────┐
   .github/** ───────▶│  ci-integration.yml  │
                      └──────────────────────┘

   push to main ─────▶ cache-warming.yml ──▶ Cachix
   PR touching ──────▶ codecov-guard.yml ──▶ rejects relaxation
   codecov.yml
```

## Design Decisions

### Why a separate `ci-integration.yml`?
`ci.yml` has an `all-checks` gate that explicitly enumerates every job it
depends on (and a step that *fails* if any job is missing from `needs:`).
Adding self-tests there would make every product PR pay their cost and
couple their failure modes to the product gate. See
[`docs/ci/integration-tests.md`](integration-tests.md) for the full
rationale.

### Why a separate `ci-metrics.yml`?
Same reason. The metrics job runs on `workflow_run` completion so it
observes `ci.yml` without participating in its critical path. If metrics
collection breaks, the product gate is unaffected.

### Why `all-checks` as a single aggregator?
GitHub branch protection only lets you require *named* status checks. A
matrix produces N checks with N names. The aggregator gives branch
protection a single, stable name to require and a single step that fails
loudly if a new job is added without updating the gate.

### Why Cachix + nix2container?
Nix gives reproducible, content-addressed builds; Cachix turns the
content-addressing into a shared cache. `nix2container` (via
`copyToDockerDaemon`) produces OCI images deterministically from Nix
derivations, which means image rebuilds are cache hits whenever inputs
are unchanged. See [`docs/binary-cache-strategy.md`](../binary-cache-strategy.md).

### Why coverage via tarpaulin, not llvm-cov?
Historical: tarpaulin's LCOV output integrates cleanly with codecov and
its behavior around `#[cfg(test)]` blocks matches what codecov/patch
rewards. See the long comment on the `Run security checks` step in
`ci.yml`.

## Branch Protection Contract

`main` requires:
- `All Checks Passed` (the `all-checks` job in `ci.yml`)
- `guard` (from `codecov-guard.yml`) when `codecov.yml` is touched

Everything else is informational.

## See Also

- [`troubleshooting.md`](troubleshooting.md) — common failures and fixes
- [`maintenance.md`](maintenance.md) — routine maintenance procedures
- [`onboarding.md`](onboarding.md) — new-maintainer ramp
- [`performance.md`](performance.md) — performance tuning
- [`security.md`](security.md) — secrets and supply-chain practices
- [`integration-tests.md`](integration-tests.md) — CI self-tests
