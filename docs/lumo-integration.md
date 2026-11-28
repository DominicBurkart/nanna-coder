# Using Nanna from Lumo Desktop

Lumo Desktop (Proton, 0.5.x) supports custom MCP servers over stdio or remote HTTP. Nanna exposes its task-delegation tools (`assign_task` and the rest, see [ARCHITECTURE.md](../ARCHITECTURE.md)) through `nanna mcp-serve`. Lumo attaches through `scripts/lumo-mcp.sh`, a thin stdio bridge to a long-lived Nanna daemon.

Lumo is an interface, not an identity. It has no special identity card of its own: every `assign_task` names one of your real identities (`rust-implementer`, `reviewer`, `docs-writer`, ...) and runs inside that card's capability envelope, exactly as it would from any other client.

Nanna only runs inside containers. The launcher does not change that: the daemon is one `podman run` container in `nanna-coder-pod`, with named volumes only, no published ports, no host bind mounts, no `--privileged`, no `--security-opt` and no `:z`/`:Z` relabelling.

## How it fits together

```
Lumo ──stdio──> scripts/lumo-mcp.sh ──exec──> scripts/lumo-bridge.py ──podman exec -i──> nanna mcp-attach ──unix socket──> nanna mcp-serve --listen
                                                          (in container nanna-daemon, socket /run/nanna/mcp.sock)
```

- One container, `nanna-daemon`, runs `nanna mcp-serve --listen /run/nanna/mcp.sock`. It owns the task manager, the queue, the leases and the results. There is no network listener: the socket is a file inside the container (mode 0600, in a 0700 directory) and is reachable only through `podman exec`.
- The named volume `nanna-state` is mounted at `/state` and holds `queue.jsonl`, leases, escalations, telemetry and `results/<task_id>.json`.
- Each Lumo session runs `scripts/lumo-bridge.py` (Python 3 standard library only), which owns a child `podman exec -i nanna-daemon /bin/nanna mcp-attach --client lumo` that copies stdin to the socket and the socket to stdout (newline-delimited JSON-RPC, nothing else on stdout). See [The connection bridge](#the-connection-bridge).
- A Lumo disconnect ends only its own connection. Running tasks continue, and their results are written to disk when they finish, so a later session (or a restart of the daemon) can read them with `get_task_status`, `get_task_result` and `list_tasks`.

`--client` is a label written to the daemon's log, nothing more. It is not authentication: anyone who can run `podman exec` into the container can claim any name. The security boundary is the container and the identity cards, not that tag.

## Prerequisites

1. `scripts/install.sh` has been run, so the `nanna-coder-pod` pod and harness image exist. The image must contain this version of `nanna` (the `--listen` and `mcp-attach` subcommands): rebuild and publish it before pointing Lumo's wrapper at this launcher, or the launch exits 4.
2. Your identity catalog exists in `~/.config/nanna/agents` (or `$XDG_CONFIG_HOME/nanna/agents`, or the directory in `NANNA_CATALOG_SRC`) and holds at least `auditor.toml`.
3. `scripts/lumo-mcp.sh --install-catalog` has been run (see [Installing the catalog](#installing-the-catalog)). The launcher refuses to start without the volume.
4. `scripts/lumo-repos.sh` has been run. It creates one podman volume per repository named by the catalog and clones the repository into it (see [Repositories](#repositories)). The launcher refuses to start if any of those volumes is missing.
5. Optional, for the cargo tools: `scripts/lumo-cargo-vendor.sh` has been run (see [Cargo tools and the sandbox](#cargo-tools-and-the-sandbox)). Without it the daemon still starts; the cargo tools are simply absent.
6. `podman` is on the `PATH` of the desktop session. Lumo is a GUI app and inherits the login session's `PATH`, not your shell's.

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
scripts/lumo-mcp.sh --status        # running?, image freshness, socket, task counts, cargo-home volume
scripts/lumo-mcp.sh --stop          # stop and remove the container; the nanna-state volume is kept
scripts/lumo-mcp.sh --restart       # stop, remove, start (picks up a new image or new settings)
scripts/lumo-mcp.sh --install-catalog
scripts/lumo-mcp.sh --print
```

Behaviours worth knowing:

- **Daemon stopped or missing**: the next launch starts the stopped container or creates a new one, waits up to `NANNA_DAEMON_WAIT_SECS` (30) for the socket, and prunes stale `git worktree` records in each mounted repository (a `git worktree prune` via `podman exec`, safe and run only when the daemon was just started).
- **Two launchers at once**: if creating the container fails because the other launcher created it first, the loser re-checks that the container exists and attaches to it.
- **Stale image**: a new `:latest` image is reported by `--status` (`image: STALE ...`) but a running daemon is never replaced silently, because that would interrupt its tasks. Run `--restart` when idle.
- **Daemon dies mid-session**: the bridge notices, errors the calls in flight, restarts the daemon with the launcher's `--start` and reconnects. See [The connection bridge](#the-connection-bridge).
- **Fail closed**: a missing config volume or repository volume exits 3 without starting anything. A daemon that does not open its socket in time exits 4.
- Messages go to stderr. Stdout carries only protocol bytes.

### The connection bridge

Lumo does not respawn a stdio server that has exited or hung: every later call would wait out Lumo's own 30 to 60 second client timeout. So the process Lumo owns is `scripts/lumo-bridge.py`, which stays alive and repairs the connection underneath it. It exits only when Lumo closes stdin (it then closes the child's stdin, waits up to 3 seconds, and terminates it) or when `podman` cannot be run at all.

- **Daemon restart or exec session lost** (`podman restart`, `--restart`, `--stop`, a crash): the child ends, and the bridge immediately answers every request still waiting for a response with a JSON-RPC error, code `-32000`, message `nanna daemon connection was lost; the daemon is being reconnected, retry the call`. Notifications in flight are dropped. It then runs `scripts/lumo-mcp.sh --start` (idempotent: starts or creates the daemon and waits for the socket), restarts the child with a 0.5 s backoff growing to 5 s, and repeats the `initialize` request Lumo originally sent (and `notifications/initialized`) with a private id. That handshake response is consumed by the bridge and never reaches Lumo, so Lumo's session is unaffected. Tool lists and task results are held by the daemon, which keeps no per-connection state beyond the handshake.
- **Requests while reconnecting** are queued for up to 20 seconds and sent once the connection is back. After 20 seconds they get the same `-32000` error. Nothing is dropped silently.
- **Host sleep and hung links**: while the connection is silent the bridge sends a JSON-RPC `ping` through it. If no reply arrives in time, the link is declared dead, the child is killed and the reconnect above happens. After a suspend the bridge also notices that the wall clock advanced more than 15 seconds beyond the monotonic clock between two one-second ticks, and probes at once instead of waiting for the next interval. Bridge pings use ids starting with `nanna-bridge/`, which are reserved: a client request using such an id is rejected with `-32600`, and ping replies are never forwarded to Lumo.
- **What Lumo should do on `-32000`**: retry the call. The error means the call may not have reached the daemon or its answer was lost; read-only calls are safe to retry, and for `assign_task` check `list_tasks` first if you need to be sure it was not already accepted.
- The bridge logs timestamped connect and reconnect lines to stderr and does not log the periodic pings.

| Variable | Default |
| --- | --- |
| `NANNA_BRIDGE_PING_INTERVAL_SECS` | `20` (silence before a ping is sent) |
| `NANNA_BRIDGE_PING_TIMEOUT_SECS` | `10` (wait for the reply, and for the re-handshake) |
| `NANNA_BRIDGE_QUEUE_SECS` | `20` |
| `NANNA_BRIDGE_BACKOFF_MIN_SECS` / `NANNA_BRIDGE_BACKOFF_MAX_SECS` | `0.5` / `5` |
| `NANNA_BRIDGE_JUMP_SECS` | `15` (clock jump that counts as suspend/resume) |
| `NANNA_BRIDGE_TICK_SECS` | `1` |
| `NANNA_BRIDGE_CLOSE_GRACE_SECS` | `3` |

`NANNA_BRIDGE_TEST_WALL_OFFSET_FILE` is a test hook that adds the number in the named file to the wall clock; leave it unset.

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

## Cargo tools and the sandbox

Plain-path tasks never execute repository code. The cargo tools (`cargo_check`, `cargo_test`, `cargo_build`) do: `build.rs`, proc macros and test binaries from the repository under test run inside the daemon container. They are therefore offered only when real containment is available, and only to a card that lists the tool by exact name (`*` and `cargo_*` never grant them).

### Vendoring dependencies

```
scripts/lumo-cargo-vendor.sh              # every repository of your catalog
scripts/lumo-cargo-vendor.sh marigold     # one repository
scripts/lumo-mcp.sh --restart             # the daemon mounts the volume only when it is created
```

`lumo-cargo-vendor.sh` runs one throwaway networked container from the harness image with the repository volumes mounted read-only and runs `cargo vendor` into the named volume `nanna-cargo-home` (override with `NANNA_CARGO_VOLUME`, read by both scripts). It is the only step with network access. Repositories are copied inside that container first, because marigold commits no `Cargo.lock`: the repository volumes are never written. Run it again whenever dependencies change.

When the volume exists, the launcher mounts it read-only at `/cargo` and sets `CARGO_HOME=/cargo`, `CARGO_NET_OFFLINE=true` and `NANNA_SANDBOX_HIDE=/repos:/etc/nanna:/state` (the repository, config and state mounts). The launcher never creates the volume and never mounts it writable. When it does not exist the daemon starts without the mount and without these variables, a one-line stderr notice names `scripts/lumo-cargo-vendor.sh`, and the cargo tools are absent; the reason is reported through `ToolRegistry::tools_unavailable()`. `--status` shows whether the volume exists and whether the running daemon was started with it:

```
cargo-home: present (nanna-cargo-home); daemon started WITHOUT it; run scripts/lumo-mcp.sh --restart to mount it
```

Because the daemon is created once, vendoring after the daemon started needs a `--restart` (it replaces only the container; `nanna-state` is kept and a running task is reported as failed with `DaemonRestarted`).

### Ephemeral lockfile

A repository that commits a `Cargo.lock` is built with `--frozen`. One without a lockfile (marigold) is built with `--offline`, and nanna gives the run an empty `Cargo.lock` owned by the task's uid that is removed afterwards, so it never reaches the patch.

### What the sandbox contains

Each cargo run goes through the `nanna-sandbox` helper, which, in order: refuses unless root, sets CPU, file-size and core rlimits, drops all groups and switches to a per-task unprivileged uid, unshares user, net, ipc, uts and mount namespaces, mounts an empty tmpfs over each hidden path, sets `PR_SET_NO_NEW_PRIVS`, changes directory and execs. The parent runs it with a cleared environment (PATH, CARGO_HOME, CARGO_NET_OFFLINE, CARGO_TARGET_DIR, HOME, TMPDIR, CARGO_INCREMENTAL, CARGO_TERM_COLOR, LANG; no tokens, no `NANNA_*`), a per-task scratch directory (mode 0700, never inside `/repos`), a wall-clock timeout (default 900 s) that kills the process group and then every process of the uid, and output capped at 256 KiB per stream. The new network namespace holds only `lo`, so the network and host loopback services such as Ollama are unreachable. If containment is unavailable (not root, no unshare, missing helper, failed self-test) the tools are not registered; nothing falls back to unsandboxed execution.

### Residual risks

- A hostile build script runs as an unprivileged uid. It cannot write the repository volumes, read root-only files, reach the network, or see `/repos`, `/etc/nanna` or `/state`.
- It can still read world-readable files in the daemon container (its own worktree, other tasks' worktrees under `/tmp`, anything mode 644/755) and `/proc/<pid>/cmdline` of daemon processes. It can write its own scratch directory and the ephemeral `Cargo.lock`.
- There is no PID namespace: stray processes are reaped by killing the uid, and uids come from a 40000-wide range, so concurrent tasks could in theory collide.
- Build and test output is untrusted text returned to the model (a prompt-injection surface), and files that `build.rs` or tests generate in the worktree are untrusted.
- Hiding `/repos` breaks build scripts that run `git` on the worktree.
- The host kernel's user-namespace attack surface is exposed to repository code.
- Heavy tiers (kani, aeneas with Lean) are not in the default image; they need a separate per-capability sandbox image.

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
| `NANNA_CARGO_VOLUME` | `nanna-cargo-home` (mounted read-only at `/cargo` only if it exists; also read by `lumo-cargo-vendor.sh`) |
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

- **A task answers as if it never saw the request ("the provided code", "no specific task is mentioned") and never edits anything**: the model's context window was too small. Ollama gives a CPU-only host 4096 tokens by default (check `context_length` in `curl localhost:11434/api/ps`), and one `read_file` of a source file overflows that, so the oldest messages, including the task, are dropped silently. Nanna asks Ollama for a window on every chat request (`options.num_ctx`): 16384 tokens by default, set with `NANNA_NUM_CTX` (2048 to 131072) on the daemon. `gemma4:e4b` never gets less than 65536 tokens: a lower setting is raised for requests to that model, and the daemon logs it. Other clients of the same Ollama that use the default window make Ollama reload the model when requests alternate; setting `OLLAMA_CONTEXT_LENGTH` in the Ollama service environment to the same value avoids that.
- **A task fails with `model_unavailable` (request timeout)**: the local model took longer than the model timeout to answer one request. On a CPU-only host that is also running builds, reading a few thousand tokens of prompt can take minutes, and reading a full 65536-token prompt can take about 32 minutes. The launcher gives each model request 3600 seconds; set `NANNA_MODEL_TIMEOUT_SECS` (10 to 7200) when starting the daemon to change it, and retry the task with a new `client_ref`.
- **`edit_file` reports several matches or none**: when `old_string` matches more than once the error lists the lines where the matches start, and the model passes one of them as `line` (the 1-based line on which the occurrence to replace starts). `line` is refused together with `replace_all`, when no occurrence starts on that line, and when two start on it. When nothing matches because `old_string` carries copied line numbers or literal `\"` or `\n`, the error says so and shows the corrected first line. Arguments are never corrected automatically.
- **A task fails with `no_progress`**: the agent made no successful `edit_file` or `write_file` in 32 model turns, or before the inactivity window ended, whichever came first. A failed edit does not count; only one that succeeded restarts both the turn count and the window. The window is checked between model requests, so a slow request finishes first. Until the cutoff, each stop without an edit gets a reminder to call a tool; a one-sentence `CANNOT:` reply ends the task at once. If the same cutoff is reached after the agent has edited files, the task does not fail: it goes on to the completion check. The window defaults to 2700 seconds (45 minutes); set `NANNA_MAX_INACTIVITY_SECS` (0 to 86400, 0 removes the window and restores the limit of two reminders; the 32-turn limit stays) when starting the daemon to change it. A tool call identical to one that already failed with the same error is answered with `identical to a call that failed; change the arguments` and that error. The failure message names the limit that ended the task (32 turns, the window, the reminder count or an empty model response), the model turns used, the number of failed edits and the last tool error. Partial changes stay available through `get_task_changes`.
- **A tool result ends in `characters cut` or in a bracketed hint, or reads `removed to save context`**: tool results reach the model as plain text and are bounded so they fit its window. `read_file` shows a header (`path, lines 1-200 of 980:`) and then the file's text without line numbers, at most 200 lines or 12000 characters, followed by a hint naming the next `start_line`. `search` shows `path:line: text`, with lines stopping at 200 characters and the whole result at 8000. `list_directory` shows one path per line, leaves out `target/` and `.git/` unless asked for them and stops at 200 entries. `edit_file`, `git_diff` and command results are plain text too; other tools answer in JSON. Every result other than `read_file` and `list_directory`, errors included, keeps its first and last 4000 characters. When the conversation still exceeds the window, the oldest results are replaced by a placeholder naming the call, keeping the last four; after that whole old turns are dropped and large results are cut. The daemon logs `context estimate` with its own estimate beside Ollama's `prompt_tokens`.
- **Connector fails to connect, stderr mentions podman**: `podman` is not on Lumo's `PATH`. Start Lumo from a shell that has it, or add it to the session environment.
- **`pod nanna-coder-pod not found`**: run `scripts/install.sh` or `podman pod start nanna-coder-pod`.
- **Launcher exits 3 saying the catalog volume is missing**: run `scripts/lumo-mcp.sh --install-catalog`.
- **`--install-catalog` says there is no `auditor.toml`**: the source directory is not your catalog; set `NANNA_CATALOG_SRC`.
- **`--install-catalog` refuses because the container exists**: run `--stop` first, install, then start again.
- **Launcher exits 3 saying a repository volume is missing**: run `scripts/lumo-repos.sh --only <name>`, or set `NANNA_REPOS` to the repositories you have.
- **Launcher exits 4, the daemon did not open its socket**: read the log lines it prints, or `podman logs nanna-daemon`. A harness image from before this feature has no `--listen`; update the image.
- **No `mcp-attach`/bridge process exists and every call times out**: Lumo respawns the connector only when it is removed and re-added or Lumo restarts; with the bridge this should not happen. If it does, check that `python3` is on Lumo's `PATH` (the launcher exits 127 without it) and read the bridge's stderr lines in Lumo's MCP log.
- **Calls fail with `-32000` for longer than a minute**: the bridge cannot bring the daemon back; run `scripts/lumo-mcp.sh --start` by hand and read what it prints.
- **`mcp-attach` says it cannot reach the daemon**: `scripts/lumo-mcp.sh --status`, then `--start`.
- **Tasks vanished after a restart**: they are in `/state/results` for 14 days; check `--status` and that the `nanna-state` volume still exists (`podman volume ls`). A task that was running when the daemon died shows as `failed` with `DaemonRestarted`.
- **`--status` shows `STALE`**: a newer image exists; `--restart` when no task you care about is running.
- **A card lists `cargo_check` but the tool is absent**: the daemon was started without the cargo-home volume (a stderr notice said so), or the volume lacks `config.toml`, or containment is unavailable. Run `scripts/lumo-mcp.sh --status`; if it says `absent` or `WITHOUT it`, run `scripts/lumo-cargo-vendor.sh` then `--restart`. The reason is in `tools_unavailable`.
- **`lumo-cargo-vendor.sh` says a repository volume is missing**: run `scripts/lumo-repos.sh --only <name>` first.
- **A cargo task fails offline with a missing crate**: a dependency changed since vendoring; run `scripts/lumo-cargo-vendor.sh <name>` again and `--restart`.
- **A `build.rs` that runs `git` fails**: `/repos` is hidden inside the sandbox by design.
- **`lumo-repos.sh` says credentials are not supported**: the repository is private or unreachable anonymously. It is skipped; use `NANNA_REPOS` to leave it out.
- **`assign_task` fails scope or repository checks**: `repo_path` must be `/repos/<name>` exactly as in the table above, and the identity's `scope.repos` must name the repository.
- **A refresh fails with a fast-forward error**: the checkout has diverged. Remove the volume (`podman volume rm nanna-repo-<name>`) and run `lumo-repos.sh --only <name>` again.
- Test the bridge without Lumo: `echo '{"jsonrpc":"2.0","id":1,"method":"tasks/list"}' | scripts/lumo-mcp.sh` prints the task list.

## Tests

`tests/integration/test-lumo-mcp.sh` checks the launcher against a stateful fake `podman`: the exact daemon argv (named volumes only, config read-only, state mounted, no ports or relabelling), the bridge argv, attach to a running daemon, restart of a stopped one, the creation race, a late or missing socket, `--start`, `--stop`, `--restart`, `--status` (including a stale image and the cargo-home volume present, absent, mounted or not), the read-only cargo mount with `CARGO_HOME`, `CARGO_NET_OFFLINE` and the sandbox hide list matching the real mounts, startup without the cargo-home volume, `lumo-cargo-vendor.sh --help` consistency, `--install-catalog` (exactly the files present, refusal without `auditor.toml`, refusal while the daemon exists, symlinks, default source), fail-closed startup and `NANNA_REPOS`.

`tests/integration/test-lumo-bridge.sh` runs `tests/integration/lumo_bridge_test.py` (also runnable alone with `python3 -I`) against a fake `podman` whose `exec` runs a fake MCP daemon: forwarding and stdout purity, immediate errors for pending ids when the child dies, transparent re-handshake, ping-timeout detection of a hung child, a simulated clock jump, queueing and its expiry, shutdown on stdin close and the private id namespace.

`tests/integration/test-lumo-repos.sh` checks `lumo-repos.sh`: the volume list derived from the catalog, clone for new volumes and fetch for existing ones, `--only` and `--list`, named-volume-only mounts, no credentials or SELinux changes in argv, cleanup after a failed clone.

`harness/tests/mcp_listen_attach.rs` runs the real binary: `mcp-serve --listen`, `mcp-attach` against it, two simultaneous clients, a client leaving, the daemon dying under a client, SIGTERM removing the socket, and results written before a restart being served afterwards. The unit tests in `harness/src/result_store.rs`, `harness/src/mcp/listen.rs` and `harness/src/mcp/attach.rs` cover the atomic, private result files, retention, socket permissions and stale-socket handling.
