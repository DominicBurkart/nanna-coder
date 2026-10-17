# PR lifecycle tools

Eight tools let an identity-scoped task work a pull request: `git_commit`, `git_push_branch`, `github_pr_open`, `github_pr_comments`, `github_pr_promote`, `github_pr_close`, `github_issue_read` and `github_issue_comment`. All are `Repository` effect class, so every call goes through the action gate.

## Granting the tools

An identity receives one of these tools only if its `[scope] tools` list names it exactly. Globs such as `*`, `git_*` or `github_*` do not grant them. The shipped `rust-implementer` identity (`git_*`) and the `evals/cases/auditor_spawn/catalog` copies of `rust-implementer` and `pr-shepherd` therefore get none of the eight; `harness/tests/fixtures/identities/global/pr-shepherd.toml` shows the explicit form:

```toml
tools = ["read_file", "write_file", "search", "list_directory", "cargo_*", "git_*", "github_pr_status", "git_commit", "git_push_branch", "github_pr_open", "github_pr_comments", "github_pr_promote", "github_pr_close", "github_issue_read", "github_issue_comment"]
```

The identity's `max_effect` must be at least `repository`. Unscoped tasks are read-only and never receive these tools.

## Credentials

Every git subprocess runs with a cleared environment plus an allow-list: PATH, HOME, USERPROFILE, HOMEDRIVE, HOMEPATH, APPDATA, LOCALAPPDATA, SYSTEMROOT, SYSTEMDRIVE, TEMP, TMP, TMPDIR, LANG, LC_ALL, XDG_CONFIG_HOME, SSH_AUTH_SOCK and USER. A push can therefore authenticate only through a credential helper reachable from HOME or XDG_CONFIG_HOME, or through SSH_AUTH_SOCK. `GITHUB_TOKEN`, `GIT_ASKPASS`, `GIT_SSH_COMMAND` and `GIT_CONFIG_*` set in the harness environment are not passed to git. The GitHub API tools read `GITHUB_TOKEN` in the harness process itself.

The credentials must not carry merge scope; no tool merges.

## Limits

The `Nanna-Identity` trailer and the PR body marker are attribution, not authentication. Repository hooks are disabled for the tools' git calls. GitHub-sourced text returned to the model is escaped, capped and fenced as untrusted data.

## Accepted characters

Tool arguments and GitHub-sourced text are checked against a printable allow-list: ASCII, letters and digits of any script, common punctuation, currency, arrows, mathematical and technical symbols, CJK and fullwidth punctuation (but not U+3000), and emoji. Context-dependent characters are accepted only in context: U+FE0F after an emoji or symbol base (or a keycap base), U+20E3 after U+FE0F or a keycap base, U+200D and U+200C only between two non-ASCII printable characters, combining marks (including Indic viramas) only after a non-space base, and a carriage return only as part of CRLF in multi-line fields. Everything else is refused in arguments and escaped as `\u{hex}` or removed in output: control characters, format characters, tag characters, variation selectors outside those contexts, no-break and other Unicode spaces, private use and unassigned code points. Combining-mark coverage is a block table, not the full Unicode general-category data.
