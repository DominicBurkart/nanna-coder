# Using Nanna from Lumo Desktop

Lumo Desktop (Proton, 0.5.x) supports custom MCP servers over stdio or remote HTTP. Nanna exposes its task-delegation tools (`assign_task` and the rest, see [ARCHITECTURE.md](../ARCHITECTURE.md)) through `nanna mcp-serve`, a stdio MCP server. Lumo attaches to it through `scripts/lumo-mcp.sh`.

Nanna only runs inside containers. The launcher does not change that: it starts the same `podman run --rm -i --pod nanna-coder-pod <harness image> /bin/nanna mcp-serve` command that `scripts/install.sh` registers for Claude Code.

## Prerequisites

1. `scripts/install.sh` has been run, so the `nanna-coder-pod` pod and harness image exist.
2. `scripts/lumo-mcp.sh --install-config` has been run from this checkout. It imports Lumo's capability config into the podman volume `nanna-lumo-config`. The launcher refuses to start without that volume.
3. `scripts/lumo-repos.sh` has been run. It creates one podman volume per repository in the `lumo` card and clones the repository into it (see [Repositories](#repositories)). The launcher refuses to start if any of those volumes is missing.
4. `podman` is on the `PATH` of the desktop session. Lumo is a GUI app and inherits the login session's `PATH`, not your shell's.

## Register the server

1. Print the values to enter:

   ```
   scripts/lumo-mcp.sh --print
   ```

2. In Lumo Desktop open **Customize > Connectors**, click **Add MCP server**, and enter the printed `name` (`nanna-coder`) and `command` (an absolute path to `scripts/lumo-mcp.sh`) with no arguments.
3. Lumo's per-action permission defaults to **Auto-approve**. Set `assign_task` and other mutating tools to **Ask human** so a chat cannot start a coding task unprompted.
4. When the connector shows as **connected**, ask Lumo to list Nanna's tools.

Lumo's configuration is UI-only, so there is no file to edit or script to automate this step.

## Repositories

The harness container mounts only the config volume, so no host path can ever resolve inside it. Repositories therefore live in podman named volumes, one per entry in the `lumo` card's `scope.repos`:

| Repository | Volume | Path inside the container |
| --- | --- | --- |
| `github.com/DominicBurkart/marigold` | `nanna-repo-marigold` | `/repos/marigold` |
| `github.com/DominicBurkart/personal-site-actix` | `nanna-repo-personal-site-actix` | `/repos/personal-site-actix` |
| `github.com/DominicBurkart/nanna-coder` | `nanna-repo-nanna-coder` | `/repos/nanna-coder` |
| `github.com/1-Track/one_track` | `nanna-repo-one_track` | `/repos/one_track` |

`scripts/lumo-repos.sh --list` prints this table from the card, so it stays correct when the card changes. The name is the lowercased last segment of the slug.

```
scripts/lumo-repos.sh                  # create and populate every volume
scripts/lumo-repos.sh --only marigold  # one repository
scripts/lumo-repos.sh --list           # name, volume, slug; no podman needed
```

Populating a new volume runs a short-lived container with that volume mounted and does an anonymous `git clone https://<slug>`, so `git remote get-url origin` equals the slug-bearing URL that `scope.repos` is matched against. Re-running fetches with `--prune`, fast-forwards the checkout and prunes stale worktree records. This is the only step with network access; the long-running server container gets none from this script. The script needs an image with `git`: it defaults to `docker.io/library/rust:1-bookworm`, overridable with `NANNA_REPOS_IMAGE`. The harness image itself currently contains no `git` executable, so `assign_task` cannot succeed inside it (it runs `git remote get-url origin` and `git worktree add`) until the image ships `git`; that is an image change outside this launcher.

Credentials are not supported. Private repositories fail the anonymous clone, the script says so, removes the empty volume it just created, skips that repository and exits 4 after processing the rest. No token is accepted, embedded in a URL or passed in argv. The `one_track` volume is therefore absent unless you populate it some other way. Start the launcher with `NANNA_REPOS=marigold,personal-site-actix,nanna-coder` to mount only the repositories you have; the card's scope check still applies server-side.

### Naming a repository in `repo_path`

Lumo must pass `repo_path` as the in-container path `/repos/<name>`, for example `{"repo_path": "/repos/marigold", "identity": "lumo", ...}`. Host paths and `~` never resolve inside the container: `repo_path` is used as given, with no mapping or expansion. This is the simplest option that needs no server change. A server-side slug-to-path mapping (accepting `github.com/DominicBurkart/marigold` and resolving it to `/repos/marigold`) would need a Rust change and is tracked as a follow-up.

### Mounts and why the volumes are writable

`assign_task` runs `git worktree add` against `repo_path`, which writes `.git/worktrees/<id>` into the base checkout. A read-only mount would make every task fail, so the repo volumes are mounted read-write. The worktrees themselves go to the container's temporary directory, so the base checkout's files are never edited. The writable surface is limited to podman named volumes that hold only a clone of a repository you already scope Lumo to.

The launcher passes `-v <volume>:<container path>[:ro]` and nothing else: no host bind mounts, no `--privileged`, no `--security-opt`, no `:z` or `:Z` relabelling and no change to SELinux labels of any host file. Podman labels its own named volumes. Credentials never go into an image or argv.

## What Lumo may do

Every `assign_task` runs under an identity card, and the card is the capability envelope. Lumo's server sees only the two cards in `scripts/lumo-config/agents/`, never your own `~/.config/nanna`:

| Card | Loop | Effect ceiling | Tools | Limits |
| --- | --- | --- | --- | --- |
| `lumo` | inner | workspace | read/write files, list, search, `git_status`, `git_diff`, `cargo_check`, `cargo_test`, `cargo_build` | 100 iterations, 1 h, one task at a time |
| `auditor` | inner | none | none | audits every spawn and action |

`lumo` has no network, and no commit, push or pull-request tools. It may touch only the repositories and paths named in its card. Widen the card in `scripts/lumo-config/agents/lumo.toml` and re-run `--install-config`; Lumo must pass `"identity": "lumo"` on every `assign_task`.

The config is mounted read-only from a podman named volume, which podman labels itself. The launcher adds no host mounts and never changes SELinux labels or container separation.

## Calling tools from Lumo

Lumo does not send the MCP Tasks `task` field, so Nanna accepts plain calls:

1. `assign_task` with `description`, `repo_path` (absolute) and `identity` (required; use `lumo`) returns `{"task_id", "status": "working", "next"}` immediately.
2. `get_task_status` with that `task_id` reports `working`, `completed`, `failed` or `cancelled`.
3. `get_task_result` with that `task_id` returns the outcome once terminal. It never blocks; while the task is working it returns the status, so call it again later.

`assign_task` acknowledges as soon as the task is registered and persisted; the spawn audit runs afterwards and can take as long as the auditor model needs, so the first status is `working` ("Auditing the request."). A request the audit refuses later reads as `failed` with the refusal reason, and nothing has run: the audit allows the task before any worktree, tool or model use. Pass a `client_ref` (1 to 128 printable characters) so a retry is safe: a second call with the same `identity` and `client_ref` returns the existing task instead of starting another.

Clients that do support Tasks keep the original flow: include `task` on `assign_task` and read `tasks/result`.

## Configuration

The launcher reads these environment variables from the Lumo process:

| Variable | Default |
| --- | --- |
| `NANNA_MODEL` | `gemma4:e4b` |
| `NANNA_POD` | `nanna-coder-pod` |
| `NANNA_REGISTRY` | `ghcr.io/dominicburkart/nanna-coder` |
| `NANNA_TAG` | `latest` |
| `NANNA_HARNESS_IMAGE` | `$NANNA_REGISTRY/harness:$NANNA_TAG` |
| `NANNA_CONFIG_VOLUME` | `nanna-lumo-config` |
| `NANNA_REPOS` | unset: every repository in the card. Comma-separated names to mount only those |
| `NANNA_REPO_VOLUME_PREFIX` | `nanna-repo-` |
| `NANNA_LUMO_CARD` | `scripts/lumo-config/agents/lumo.toml` |
| `NANNA_REPOS_IMAGE` | `docker.io/library/rust:1-bookworm` (used by `lumo-repos.sh` only) |

## Troubleshooting

- **Connector fails to connect, stderr mentions podman**: `podman` is not on Lumo's `PATH`. Start Lumo from a shell that has it, or add it to the session environment.
- **`pod nanna-coder-pod not found`**: run `scripts/install.sh` or `podman pod start nanna-coder-pod`.
- **Launcher exits 3 saying a repository volume is missing**: run `scripts/lumo-repos.sh --only <name>`, or set `NANNA_REPOS` to the repositories you have.
- **`lumo-repos.sh` says credentials are not supported**: the repository is private or unreachable anonymously. It is skipped; use `NANNA_REPOS` to leave it out.
- **`assign_task` fails scope or repository checks**: `repo_path` must be `/repos/<name>` exactly as in the table above, not a host path.
- **A refresh fails with a fast-forward error**: the checkout has diverged. Remove the volume (`podman volume rm nanna-repo-<name>`) and run `lumo-repos.sh --only <name>` again.
- Test the launcher without Lumo: `echo '{"jsonrpc":"2.0","id":1,"method":"ping"}' | scripts/lumo-mcp.sh`.

## Tests

`tests/integration/test-lumo-repos.sh` checks `lumo-repos.sh` against a fake `podman`: the volumes derived from the card, clone for new volumes and fetch for existing ones, `--only` and `--list`, named-volume-only mounts, no credentials or SELinux changes in argv, cleanup after a failed clone.

`tests/integration/test-lumo-mcp.sh` checks the launcher against a fake `podman`: exact argv, environment overrides, stdin passthrough, `--print`, the missing-podman error, fail-closed startup without the config volume or any repo volume, `NANNA_REPOS`, named-volume-only mounts, the exact contents of `--install-config`, and the `lumo` card's envelope.
