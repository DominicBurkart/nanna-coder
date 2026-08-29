# CI Onboarding

If you are new to Nanna Coder's CI, read the files in this order. Each item is either a doc in this tree or an existing top-level doc that we intentionally do not duplicate.

## Read in this order

1. **[architecture.md](architecture.md)** — the authoritative inventory of workflows, jobs, matrices, secrets, and the `ci.yml` job graph. Every other doc in this tree references it.
2. **[../../README.md](../../README.md)** — one-page project overview and quick start. Confirms the "nix develop" entry point.
3. **[../../AGENTS.md](../../AGENTS.md)** — agent operating instructions, useful even if you are human: it describes the state machine the harness follows and which gates exist.
4. **[../../TESTING.md](../../TESTING.md)** — test topology and the 100% patch-coverage gate (relaxed to 90% in [../../codecov.yml](../../codecov.yml) with documented reasons). Important background for understanding `test-matrix`.
5. **[../../CONTRIBUTING.md](../../CONTRIBUTING.md)** — PR conventions and issue-closing rules.
6. **[../ci-cd-pipeline.md](../ci-cd-pipeline.md)** — narrative, higher-level view of the pipeline from before this tree existed. Some sections are now superseded by [architecture.md](architecture.md); when they conflict, architecture.md wins because it is verified by scripts.
7. **Cache story, in order:**
   - [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) — current operational cache keys and workflow wiring
   - [../binary-cache-strategy.md](../binary-cache-strategy.md) — high-level cache architecture
   - [../cachix-migration.md](../cachix-migration.md) — why we are on Cachix
   - [../cache-migration-guide.md](../cache-migration-guide.md) — historical migration notes (superseded but retained)
   - [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md) — how to get push access and what the public key is
8. **[../developer-experience.md](../developer-experience.md)** — `nix develop` aliases, `dev-check`, `container-dev`, etc. Many CI failures reproduce faster with these.
9. **[../../ARCHITECTURE.md](../../ARCHITECTURE.md)** — the harness state machine. You do not need this to fix CI, but knowing what gets tested helps.

After that, skim:

- [troubleshooting.md](troubleshooting.md) — know where to look when the pipeline screams
- [maintenance.md](maintenance.md) — know what upkeep you are inheriting
- [performance.md](performance.md) — know which jobs dominate wall-clock time
- [security.md](security.md) — know which secrets and permissions exist

## Mental model

```
                +----------------------+
  PR / push --> |       ci.yml         | <-- the merge gate (all-checks)
                +----------------------+
                          |
  push to main  ------->  cache-warming.yml  (pre-populates Cachix)
                          |
  manual dispatch ----->  eval.yml           (agent evaluation, not a PR gate)
                          |
  push to main  ------->  badges.yaml        (decorative; does not gate merges)
```

Only `ci.yml` blocks merges. The others run alongside it.

## First useful tasks

- Reproduce a `test-matrix` failure locally: `nix develop --command cargo nextest run --workspace --all-features`
- Reproduce a `lint` failure: `nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings`
- Reproduce the `security` variant: `nix develop --command cargo tarpaulin --skip-clean --ignore-tests --out Lcov --output-dir . --timeout 1800`
- Reproduce a container build: `nix build .#harnessImage`
- Run the two docs-check scripts before pushing: `bash scripts/check-docs-links.sh && bash scripts/check-ci-doc-coverage.sh`

## Where to ask

- Workflow behavior — the file itself under `.github/workflows/` is the source of truth; read it before opening an issue.
- Cache oddities — [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) first, then open an issue.
- Doc drift — either fix it in this tree, or open an issue if the fix is more than a line.

## Related documents

- [architecture.md](architecture.md)
- [troubleshooting.md](troubleshooting.md)
- [maintenance.md](maintenance.md)
- [performance.md](performance.md)
- [security.md](security.md)
- [../ci-cd-pipeline.md](../ci-cd-pipeline.md)
- [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md)
- [../binary-cache-strategy.md](../binary-cache-strategy.md)
- [../cache-migration-guide.md](../cache-migration-guide.md)
- [../cachix-migration.md](../cachix-migration.md)
- [../developer-experience.md](../developer-experience.md)
- [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md)
- [../../ARCHITECTURE.md](../../ARCHITECTURE.md)
- [../../TESTING.md](../../TESTING.md)
- [../../CONTRIBUTING.md](../../CONTRIBUTING.md)
- [../../AGENTS.md](../../AGENTS.md)
