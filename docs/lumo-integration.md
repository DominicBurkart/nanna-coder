# Using Nanna from Lumo Desktop

Lumo Desktop (Proton, 0.5.x) supports custom MCP servers over stdio or remote HTTP. Nanna exposes its task-delegation tools (`assign_task` and the rest, see [ARCHITECTURE.md](../ARCHITECTURE.md)) through `nanna mcp-serve`. Lumo attaches through `scripts/lumo-mcp.sh`, a thin stdio bridge to a long-lived Nanna daemon.

Lumo is an interface, not an identity. It has no special identity card of its own: every `assign_task` names one of your real identities (`rust-implementer`, `reviewer`, `docs-writer`, ...) and runs inside that card's capability envelope, exactly as it would from any other client.

Nanna only runs inside containers. The launcher does not change that: the daemon is one `podman run` container in `nanna-coder-pod`, with named volumes only, no published ports, no host bind mounts, no `--privileged`, no `--security-opt` and no `:z`/`:Z` relabelling.

## How it fits together

```
Lumo ──stdio──> scripts/lumo-mcp.sh ──podman exec -i──> nanna mcp-attach ──unix socket──> nanna mcp-serve --listen
                                                          (in container nanna-daemon, socket /run/nanna/mcp.sock)
```

- One container, `nanna-daemon`, runs `nanna mcp-serve --listen /run/nanna/mcp.sock`. It owns the task manager, the queue, the leases and the results. There is no network listener: the socket is a file inside the container (mode 0600, in a 0700 directory) and is reachable only through `podman exec`.
- The named volume `nanna-state` is mounted at `/state` and holds `queue.jsonl`, leases, escalations, telemetry and `results/<task_id>.json`.
- Each Lumo session runs `podman exec -i nanna-daemon /bin/nanna mcp-attach --client lumo`, which copies stdin to the socket and the socket to stdout (newline-delimited JSON-RPC, nothing else on stdout).
- A Lumo disconnect ends only its own connection. Running tasks continue, and their results are written to disk when they finish, so a later session (or a restart of the daemon) can read them with `get_task_status`, `get_task_result` and `list_tasks`.

`--client` is a label written to the daemon's log, nothing more. It is not authentication: anyone who can run `podman exec` into the container can claim any name. The security boundary is the container and the identity cards, not that tag.

## Prerequisites

1. `scripts/install.sh` has been run, so the `nanna-coder-pod` pod and harness image exist. The image must contain this version of `nanna` (the `--listen` and `mcp-attach` subcommands): rebuild and publish it before pointing Lumo's wrapper at this launcher, or the launch exits 4.
2. Your identity catalog exists in `~/.config/nanna/agents` (or `$XDG_CONFIG_HOME/nanna/agents`, or the directory in `NANNA_CATALOG_SRC`) and holds at least `auditor.toml`.
3. `scripts/lumo-mcp.sh --install-catalog` has been run (see [Installing the catalog](#installing-the-catalog)). The launcher refuses to start without the volume.
4. `scripts/lumo-repos.sh` has been run. It creates one podman volume per repository named by the catalog and clones the repository into it (see [Repositories](#repositories)). The launcher refuses to start if any of those volumes is missing.
5. `podman` is on the `PATH` of the desktop session. Lumo is a GUI app and inherits the login session's `PATH`, not your shell's.

## Installing the catalog

```
scripts/lumo-mcp.sh --install-catalog
```

This copies your real base catalog into the podman volume `nanna-lumo-config` with `tar | podman volume import` (no host mount, no relabel). The source is `$NANNA_CATALOG_SRC`, else `$XDG_CONFIG_HOME/nanna/agents`, else `~/.config/nanna/agents`. Symlinked files are copied as regular files, so a catalog managed by a tool that symlinks into a store installs correctly. The command refuses if the source has no `auditor.toml`, prints the identities it installed, and refuses while the `nanna-daemon` container exists (replacing the volume would remove a container that mounts it): run `--stop` first. Repo-local `.nanna/agents` cards arrive with the repository volumes and need no extra step. Run `--install-catalog` again whenever you change a card, then `--restart`.

The config volume is mounted read-only into the daemon.

## Register the server

1. Print the values to enter:

   ```
   scripts/lumo-mcp.sh --print
   ```

2. In Lumo Desktop open **Customize > Connectors**, click **Add MCP server**, and enter the printed `name` (`nanna-coder`) and `command` with no arguments.
3. Lumo's per-action permission defaults to **Auto-approve**. Set `assign_task` and other mutating tools to **Ask human** so a chat cannot start a coding task unprompted.
4. When the connector shows as **connected**, ask Lumo to list Nanna's tools.

Lumo's configuration is UI-only, so there is no file to edit or script to automate this step.

### Updating an installed wrapper

If Lumo was set up with a wrapper such as `~/.local/bin/nanna-lumo-mcp` that exports `NANNA_REPOS` and execs an older copy of the launcher (for example `~/.local/share/nanna-lumo/lumo-mcp.sh`), this change does not edit it. Point it at the new launcher:

```
#!/usr/bin/env bash
export NANNA_REPOS=marigold,personal-site-actix,nanna-coder
exec /path/to/nanna-coder/scripts/lumo-mcp.sh "$@"
```

The wrapper must `exec` the launcher with no extra stdout output and pass its arguments through, so `nanna-lumo-mcp --status`, `--restart` and `--stop` also work. A copied launcher under `~/.local/share/nanna-lumo/` must be refreshed together with `lumo-repos.sh`, which sits next to it. Because the launcher keeps the daemon alive between sessions, `NANNA_REPOS` (and any other variable below) only takes effect when the daemon is created: run `--restart` after changing them.

## The daemon

```
scripts/lumo-mcp.sh                 # what Lumo runs: ensure the daemon, then bridge stdio to it
scripts/lumo-mcp.sh --start         # ensure the daemon is running and its socket is open
scripts/lumo-mcp.sh --status        # running?, image freshness, socket, task counts
scripts/lumo-mcp.sh --stop          # stop and remove the container; the nanna-state volume is kept
scripts/lumo-mcp.sh --restart       # stop, remove, start (picks up a new image or new settings)
scripts/lumo-mcp.sh --install-catalog
scripts/lumo-mcp.sh --print
```

Behaviours worth knowing:

- **Daemon stopped or missing**: the next launch starts the stopped container or creates a new one, waits up to `NANNA_DAEMON_WAIT_SECS` (30) for the socket, and prunes stale `git worktree` records in each mounted repository (a `git worktree prune` via `podman exec`, safe and run only when the daemon was just started).
- **Two launchers at once**: if creating the container fails because the other launcher created it first, the loser re-checks that the container exists and attaches to it.
- **Stale image**: a new `:latest` image is reported by `--status` (`image: STALE ...`) but a running daemon is never replaced silently, because that would interrupt its tasks. Run `--restart` when idle.
- **Daemon dies mid-session**: `mcp-attach` exits non-zero, the launcher exits non-zero, and Lumo reconnects; the next launch restarts the daemon.
- **Fail closed**: a missing config volume or repository volume exits 3 without starting anything. A daemon that does not open its socket in time exits 4.
- Messages go to stderr. Stdout carries only protocol bytes.

### Results and restarts

When a task reaches a terminal state (completed, failed, cancelled) its `Task` record is written to `/state/results/<task_id>.json` (the state directory is `NANNA_STATE_DIR`, defaulting to the queue file's directory). Files are created mode 0600 in a 0700 directory by writing a temporary file and renaming it; only regular files are read, and symlinks are never followed. At start the daemon reloads them, so `list_tasks`, `get_task_status` and `get_task_result` answer for old tasks. Records older than 14 days (`NANNA_RESULT_RETENTION_DAYS`) are pruned at start and daily. A record is the full task, including its patch, tool-call history and, for a failed task, the conversation snapshot, so treat the `nanna-state` volume as sensitive as the repositories themselves.

If the daemon dies while a task is running, the task is reported as `failed` with `error_type: "DaemonRestarted"` after the next start. It is safe to resubmit. Tasks that were still queued or being audited are re-audited and run by the existing restore path.

## Repositories

The daemon mounts only named volumes, so no host path can ever resolve inside it. Repositories live in podman named volumes, one per repository named in the `scope.repos` of the catalog's cards (the de-duplicated union across `*.toml` in the catalog directory):

| Repository | Volume | Path inside the container |
| --- | --- | --- |
| `github.com/DominicBurkart/marigold` | `nanna-repo-marigold` | `/repos/marigold` |
| `github.com/DominicBurkart/personal-site-actix` | `nanna-repo-personal-site-actix` | `/repos/personal-site-actix` |
| `github.com/DominicBurkart/nanna-coder` | `nanna-repo-nanna-coder` | `/repos/nanna-coder` |
| `github.com/1-Track/one_track` | `nanna-repo-one_track` | `/repos/one_track` |

`scripts/lumo-repos.sh --list` prints this table from your catalog. The name is the lowercased last segment of the slug.

```
scripts/lumo-repos.sh                  # create and populate every volume
scripts/lumo-repos.sh --only marigold  # one repository
scripts/lumo-repos.sh --list           # name, volume, slug; no podman needed
```

Populating a new volume runs a short-lived container with that volume mounted and does an anonymous `git clone https://<slug>`, so `git remote get-url origin` equals the slug-bearing URL that `scope.repos` is matched against. Re-running fetches with `--prune`, fast-forwards the checkout and prunes stale worktree records. This is the only step with network access; the long-running daemon gets none from this script. The script needs an image with `git`: it defaults to `docker.io/library/rust:1-bookworm`, overridable with `NANNA_REPOS_IMAGE`.

Credentials are not supported. Private repositories fail the anonymous clone, the script says so, removes the empty volume it just created, skips that repository and exits 4 after processing the rest. No token is accepted, embedded in a URL or passed in argv. The `one_track` volume is therefore absent unless you populate it some other way. Start the launcher with `NANNA_REPOS=marigold,personal-site-actix,nanna-coder` to mount only the repositories you have; the card's scope check still applies server-side.

### Naming a repository in `repo_path`

Lumo must pass `repo_path` as the in-container path `/repos/<name>`, for example `{"repo_path": "/repos/marigold", "identity": "rust-implementer", ...}`. Host paths and `~` never resolve inside the container: `repo_path` is used as given, with no mapping or expansion.

### Mounts and why the volumes are writable

`assign_task` runs `git worktree add` against `repo_path`, which writes `.git/worktrees/<id>` into the base checkout. A read-only mount would make every task fail, so the repo volumes are mounted read-write. The worktrees themselves go to the container's temporary directory, so the base checkout's files are never edited. The writable surface is limited to podman named volumes that hold only a clone of a repository you already scope an identity to.

The launcher passes `-v <volume>:<container path>[:ro]` and nothing else. Podman labels its own named volumes. Credentials never go into an image or argv.

## What Lumo may do

Every `assign_task` runs under an identity card from your catalog, and the card is the capability envelope: its repositories, paths, effect ceiling, tools and limits. Lumo must pass `"identity": "<name>"` on every `assign_task`; it gets no more and no less than that card allows. `auditor` audits every spawn and action. Which identities exist is decided by your `~/.config/nanna/agents`, not by this repository; to change what Lumo can do, edit the card, run `--install-catalog` and `--restart`.

## Calling tools from Lumo

Lumo does not send the MCP Tasks `task` field, so Nanna accepts plain calls:

1. `assign_task` with `description`, `repo_path` (absolute) and `identity` (required) returns `{"task_id", "status": "working", "next"}` immediately.
2. `get_task_status` with that `task_id` reports `working`, `completed`, `failed` or `cancelled`.
3. `get_task_result` with that `task_id` returns the outcome once terminal. It never blocks; while the task is working it returns the status, so call it again later, from this session or a later one.

`assign_task` acknowledges as soon as the task is registered and persisted; the spawn audit runs afterwards and can take as long as the auditor model needs, so the first status is `working` ("Auditing the request."). A request the audit refuses later reads as `failed` with the refusal reason, and nothing has run. Pass a `client_ref` (1 to 128 printable characters) so a retry is safe within the daemon's lifetime: a second call with the same `identity` and `client_ref` returns the existing task instead of starting another. The `client_ref` index is not part of the persisted task record, so after a daemon restart a finished task is still readable by its `task_id` but a retry with its `client_ref` starts a new task.

Clients that do support Tasks keep the original flow: include `task` on `assign_task` and read `tasks/result`.

## Configuration

The launcher reads these environment variables. Those that shape the container only apply when the daemon is created (use `--restart`).

| Variable | Default |
| --- | --- |
| `NANNA_MODEL` | `gemma4:e4b` |
| `NANNA_POD` | `nanna-coder-pod` |
| `NANNA_REGISTRY` | `ghcr.io/dominicburkart/nanna-coder` |
| `NANNA_TAG` | `latest` |
| `NANNA_HARNESS_IMAGE` | `$NANNA_REGISTRY/harness:$NANNA_TAG` |
| `NANNA_CONFIG_VOLUME` | `nanna-lumo-config` |
| `NANNA_STATE_VOLUME` | `nanna-state` |
| `NANNA_DAEMON_NAME` | `nanna-daemon` |
| `NANNA_DAEMON_SOCKET` | `/run/nanna/mcp.sock` |
| `NANNA_DAEMON_WAIT_SECS` | `30` |
| `NANNA_AUDIT_TIMEOUT_SECS` | `300` (passed to the daemon) |
| `NANNA_RESULT_RETENTION_DAYS` | unset: the daemon's default of 14 |
| `NANNA_CATALOG_SRC` | `$XDG_CONFIG_HOME/nanna/agents`, else `~/.config/nanna/agents` |
| `NANNA_REPOS` | unset: every repository in the catalog. Comma-separated names to mount only those |
| `NANNA_REPO_VOLUME_PREFIX` | `nanna-repo-` |
| `NANNA_REPOS_IMAGE` | `docker.io/library/rust:1-bookworm` (used by `lumo-repos.sh` only) |

## Troubleshooting

- **Connector fails to connect, stderr mentions podman**: `podman` is not on Lumo's `PATH`. Start Lumo from a shell that has it, or add it to the session environment.
- **`pod nanna-coder-pod not found`**: run `scripts/install.sh` or `podman pod start nanna-coder-pod`.
- **Launcher exits 3 saying the catalog volume is missing**: run `scripts/lumo-mcp.sh --install-catalog`.
- **`--install-catalog` says there is no `auditor.toml`**: the source directory is not your catalog; set `NANNA_CATALOG_SRC`.
- **`--install-catalog` refuses because the container exists**: run `--stop` first, install, then start again.
- **Launcher exits 3 saying a repository volume is missing**: run `scripts/lumo-repos.sh --only <name>`, or set `NANNA_REPOS` to the repositories you have.
- **Launcher exits 4, the daemon did not open its socket**: read the log lines it prints, or `podman logs nanna-daemon`. A harness image from before this feature has no `--listen`; update the image.
- **`mcp-attach` says it cannot reach the daemon**: `scripts/lumo-mcp.sh --status`, then `--start`.
- **Tasks vanished after a restart**: they are in `/state/results` for 14 days; check `--status` and that the `nanna-state` volume still exists (`podman volume ls`). A task that was running when the daemon died shows as `failed` with `DaemonRestarted`.
- **`--status` shows `STALE`**: a newer image exists; `--restart` when no task you care about is running.
- **`lumo-repos.sh` says credentials are not supported**: the repository is private or unreachable anonymously. It is skipped; use `NANNA_REPOS` to leave it out.
- **`assign_task` fails scope or repository checks**: `repo_path` must be `/repos/<name>` exactly as in the table above, and the identity's `scope.repos` must name the repository.
- **A refresh fails with a fast-forward error**: the checkout has diverged. Remove the volume (`podman volume rm nanna-repo-<name>`) and run `lumo-repos.sh --only <name>` again.
- Test the bridge without Lumo: `echo '{"jsonrpc":"2.0","id":1,"method":"tasks/list"}' | scripts/lumo-mcp.sh` prints the task list.

## Tests

`tests/integration/test-lumo-mcp.sh` checks the launcher against a stateful fake `podman`: the exact daemon argv (named volumes only, config read-only, state mounted, no ports or relabelling), the bridge argv, attach to a running daemon, restart of a stopped one, the creation race, a late or missing socket, `--start`, `--stop`, `--restart`, `--status` (including a stale image), `--install-catalog` (exactly the files present, refusal without `auditor.toml`, refusal while the daemon exists, symlinks, default source), fail-closed startup and `NANNA_REPOS`.

`tests/integration/test-lumo-repos.sh` checks `lumo-repos.sh`: the volume list derived from the catalog, clone for new volumes and fetch for existing ones, `--only` and `--list`, named-volume-only mounts, no credentials or SELinux changes in argv, cleanup after a failed clone.

`harness/tests/mcp_listen_attach.rs` runs the real binary: `mcp-serve --listen`, `mcp-attach` against it, two simultaneous clients, a client leaving, the daemon dying under a client, SIGTERM removing the socket, and results written before a restart being served afterwards. The unit tests in `harness/src/result_store.rs`, `harness/src/mcp/listen.rs` and `harness/src/mcp/attach.rs` cover the atomic, private result files, retention, socket permissions and stale-socket handling.
