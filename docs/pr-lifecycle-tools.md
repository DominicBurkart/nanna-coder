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

Tool arguments and GitHub-sourced text are checked character by character against one function (`unprintable_mask`) that validation, escaping and stripping all share. A character passes if it is ASCII graphic or a space, any letter or digit Rust classes as alphanumeric, a character in the allowed punctuation, currency, arrow, mathematical, technical, symbol, CJK-punctuation (not U+3000), fullwidth (U+FF01-FF60) and emoji ranges, or one of the context-dependent characters below. Unassigned code points inside the allowed ranges and blocks do pass; the list is an allow-list of ranges, not of assigned characters.

Context-dependent characters:
- U+FE0F is accepted after an emoji or symbol base, and after a digit, `#` or `*` only when U+20E3 follows; U+20E3 is accepted after U+FE0F or a bare digit, `#` or `*`.
- U+200D and U+200C are accepted only between two non-ASCII printable characters, and the following character must not be a combining mark. This is a documented limit: a joiner between two non-ASCII characters is invisible and is accepted.
- Combining marks (every Mn, Mc and Me code point of Unicode 15.1 except U+034F, the variation selectors, U+17B4, U+17B5, U+180B-U+180E and U+20E3) are accepted only directly after an alphabetic character or another accepted mark, and at most four in a row. Marks after digits, spaces, punctuation or a stripped character are refused. Code points added after Unicode 15.1 are not in the table.
- A carriage return is accepted only as part of CRLF in multi-line fields and is dropped from output.

Everything not accepted is refused in arguments. In output it is removed (git and error text, which are stripped before secret redaction and re-checked until stable) or written as `\u{hex}` (GitHub text). Escaped output is capped by its escaped length. Known over-rejections are tracked in the follow-up issue linked from the pull request.
