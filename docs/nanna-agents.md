# nanna agent identities

nanna spawns agents from human-authored identity cards. This repository ships
four cards scoped to its layout, a base catalog they narrow, and a check that
parses both with nanna's own parser.

## Files

| Path | Role |
| --- | --- |
| `.nanna/agents/*.toml` | Repo-local cards. Each names a base card and may only narrow it. |
| `.nanna/catalog/*.toml` | Reference copy of the base catalog (including the required `auditor`). |
| `.nanna/catalog/prompts/` | System prompts for the base cards. |
| `scripts/check_nanna_agents.sh` | Parses both layers with nanna and fails on a widening override. |

`.nanna/**` is in nanna's fixed protected set for every repository, so an
agent cannot edit these cards through `write_file`, and patch extraction
refuses a patch that touches them.

## Why there is a base catalog

nanna merges `.nanna/agents/` over a host-owned global catalog
(`$NANNA_CONFIG_DIR/agents`, else `$XDG_CONFIG_HOME/nanna/agents`, else
`~/.config/nanna/agents`). A repo-local card must share its name with a global
card and may only narrow it: `scope.max_effect` and every `[limits]` value at
or below the base; `scope.repos`, `scope.paths`, `scope.tools` and
`scope.read_paths` string-subsets of the base lists. A name with no global card
is rejected, and `auditor` cannot be overridden at all.

nanna's shipped fixtures name `github.com/example/repo`, so they cannot be a
base for this repository. `.nanna/catalog/` is the authored base.
Installing it is a human step on the host that runs nanna:

```text
mkdir -p ~/.config/nanna/agents
cp -r .nanna/catalog/. ~/.config/nanna/agents/
```

## Identities

Names are exact; inchworm flows reference them verbatim.

| Name | Loop | Flow step | Writes (`paths`) | Ceiling | Tools |
| --- | --- | --- | --- | --- | --- |
| `rust-implementer` | inner | implement | `harness/**`, `image-builder/**`, `model/**`, `framing/**`, `evals/**`, `tests/**` | workspace | `read_file`, `write_file`, `list_directory`, `search`, `git_status`, `git_diff`, `cargo_check`, `cargo_test` |
| `pr-shepherd` | middle | publish | same trees as `rust-implementer` | repository | the above plus `git_commit`, `git_push_branch`, `github_pr_open`, `github_pr_status`, `github_pr_comments`, `github_pr_close`, `github_issue_read`, `github_issue_comment` |
| `reviewer` | middle | review | none | none | `read_file`, `list_directory`, `search`, `git_status`, `git_diff` |
| `docs-writer` | inner | docs | `docs/**/*.md`, `README.md`, `ARCHITECTURE.md`, `TESTING.md` | workspace | `read_file`, `write_file`, `list_directory`, `search`, `git_status`, `git_diff` |

All four read the workspace trees, `nanna-effects/**`, `docs/**`, the root
`Cargo.toml` and `Cargo.lock`, and the root markdown guides.

`rust-implementer` has no network: its ceiling is workspace and it holds no
`github_*` or `run_command` tool. `pr-shepherd` is the only card that reaches
the repository effect class; the pull-request tools it lists are the ones on
nanna main. `pr-shepherd` has no `github_pr_promote`: nanna-coder has no
solo gate, and only the maintainer marks its pull requests ready. `docs-writer` is limited to markdown by using `docs/**/*.md`
instead of `docs/**`, so the yaml, python and evidence files under `docs/` are
not writable. `AGENTS.md` is not in its write paths because it is a protected
file. Because every card restricts paths, none receives `run_command`.
Tool names are exact because the narrowing check compares strings, so
`cargo_*` in a base would not cover `cargo_check` in an override.

The cargo workspace builds `nanna-effects` too, but it is readable only; no
card writes it.

## No deployer

nanna-coder has no deploy path: there is no `.nanna/deploy.toml`, no sandbox or
production environment, and nothing it rolls out. `scripts/deploy.sh` brings up
a local container stack and is not a rollout. No card reaches the Ci, Sandbox
or Production effect classes, so there is no `deployer` or `incident-responder`
card.

## Listing

```text
NANNA_CONFIG_DIR=<dir containing agents/> nanna agents list --repo /path/to/nanna-coder
```

The repository is passed with `--repo`; nanna does not read the working
directory. The table shows name, loop, model and ceiling only; paths, reads and
tools are in the files above.

## Verifying scope

`scripts/check_nanna_agents.sh` (needs a `nanna` binary, or `NANNA_BIN`) copies
the base
catalog to a scratch config directory and runs `nanna agents list --repo .`;
it exits non-zero when an override widens its base, names an unknown card, or
fails to parse. It is not wired into CI.

## Containers

Cards carry no container field (DominicBurkart/nanna-coder#1012); the container
a task runs in is decided by the repository, not the card. nanna runs a task in
a dev container only when the repository has both `flake.nix` and a
`.devcontainer/` directory (`harness/src/task.rs` checks only that both paths
exist, so removing the `.devcontainer/` marker sends this repository's own
tasks down the non-container path); nothing else is read from `.devcontainer/`, and
`onboard_repo` generates only `flake.nix`. This repository has both
(`.devcontainer/devcontainer.json` is a minimal marker, `flake.nix` exposes
`devContainerImage`), so its tasks run through the container model. The
container model is not bypassed: tools that need the
container registry, including the `cargo_*` tools, are available only to a task
run through it.

## Limits

- `[limits]` (`max_iterations`, `max_wall_clock_secs`, `max_concurrent`),
  `model`, `scope.repos` and system prompts are validated at load time but are
  not enforced at runtime (DominicBurkart/nanna-coder#733, open). Narrowing
  them in a card documents intent; it does not constrain a running task.
- Repo-local `.nanna/agents/` overrides are applied by `mcp-serve`, `delegate`
  and task assignment on nanna main (DominicBurkart/nanna-coder#931, closed by
  #935; overrides are read from the host working tree, not a git ref). A
  `nanna` binary built before #935 loads only the global catalog and runs the
  base card rather than the narrowed one, so keep the base catalog as tight as
  you need until the installed binary is rebuilt.
