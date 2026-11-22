# Using Nanna from Lumo Desktop

Lumo Desktop (Proton, 0.5.x) supports custom MCP servers over stdio or remote HTTP. Nanna exposes its task-delegation tools (`assign_task` and the rest, see [ARCHITECTURE.md](../ARCHITECTURE.md)) through `nanna mcp-serve`, a stdio MCP server. Lumo attaches to it through `scripts/lumo-mcp.sh`.

Nanna only runs inside containers. The launcher does not change that: it starts the same `podman run --rm -i --pod nanna-coder-pod <harness image> /bin/nanna mcp-serve` command that `scripts/install.sh` registers for Claude Code.

## Prerequisites

1. `scripts/install.sh` has been run, so the `nanna-coder-pod` pod and harness image exist.
2. `scripts/lumo-mcp.sh --install-config` has been run from this checkout. It imports Lumo's capability config into the podman volume `nanna-lumo-config`. The launcher refuses to start without that volume.
3. `podman` is on the `PATH` of the desktop session. Lumo is a GUI app and inherits the login session's `PATH`, not your shell's.

## Register the server

1. Print the values to enter:

   ```
   scripts/lumo-mcp.sh --print
   ```

2. In Lumo Desktop open **Customize > Connectors**, click **Add MCP server**, and enter the printed `name` (`nanna-coder`) and `command` (an absolute path to `scripts/lumo-mcp.sh`) with no arguments.
3. Lumo's per-action permission defaults to **Auto-approve**. Set `assign_task` and other mutating tools to **Ask human** so a chat cannot start a coding task unprompted.
4. When the connector shows as **connected**, ask Lumo to list Nanna's tools.

Lumo's configuration is UI-only, so there is no file to edit or script to automate this step.

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

## Troubleshooting

- **Connector fails to connect, stderr mentions podman**: `podman` is not on Lumo's `PATH`. Start Lumo from a shell that has it, or add it to the session environment.
- **`pod nanna-coder-pod not found`**: run `scripts/install.sh` or `podman pod start nanna-coder-pod`.
- Test the launcher without Lumo: `echo '{"jsonrpc":"2.0","id":1,"method":"ping"}' | scripts/lumo-mcp.sh`.

## Tests

`tests/integration/test-lumo-mcp.sh` checks the launcher against a fake `podman`: exact argv, environment overrides, stdin passthrough, `--print`, the missing-podman error, fail-closed startup without the config volume, the exact contents of `--install-config`, and the `lumo` card's envelope.
