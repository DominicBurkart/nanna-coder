use serde_json::Value;
use std::collections::{HashMap, HashSet};

const NETWORK_BINARIES: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "socat", "scp", "sftp", "ssh", "ftp", "telnet",
    "rsync", "nmap", "tftp", "lftp", "aria2c", "mosh", "http", "https", "xh",
];

const PRIVILEGE_BINARIES: &[&str] = &["sudo", "su", "doas", "pkexec", "runuser", "setpriv"];

const DISK_BINARIES: &[&str] = &["dd", "shred", "fdisk", "parted", "wipefs"];

const SYSTEM_BINARIES: &[&str] = &[
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "telinit",
    "systemctl",
    "service",
    "mount",
    "umount",
    "chroot",
    "nsenter",
    "unshare",
    "crontab",
    "at",
    "batch",
    "useradd",
    "usermod",
    "userdel",
    "passwd",
    "iptables",
    "nft",
    "modprobe",
    "insmod",
    "pkill",
    "killall",
    "skill",
    "busybox",
    "parallel",
    "watch",
    "script",
    "flock",
    "strace",
    "ltrace",
    "gdb",
    "systemd-run",
    "visudo",
];

const PUBLISH_BINARIES: &[&str] = &[
    "twine",
    "hub",
    "glab",
    "aws",
    "gcloud",
    "az",
    "kubectl",
    "helm",
    "terraform",
    "tofu",
    "scw",
    "doctl",
    "flyctl",
    "fly",
    "heroku",
    "vercel",
    "netlify",
    "skopeo",
    "oras",
];

const PUBLISH_SUBCOMMANDS: &[(&str, usize, &[&str])] = &[
    ("cargo", 1, &["publish", "yank", "owner", "login", "logout"]),
    (
        "npm",
        2,
        &[
            "publish",
            "unpublish",
            "deprecate",
            "dist-tag",
            "login",
            "adduser",
            "token",
            "owner",
            "access",
            "logout",
        ],
    ),
    (
        "pnpm",
        2,
        &[
            "publish",
            "unpublish",
            "deprecate",
            "login",
            "adduser",
            "token",
            "owner",
        ],
    ),
    (
        "yarn",
        2,
        &["publish", "unpublish", "deprecate", "login", "npm", "token"],
    ),
    ("bun", 2, &["publish", "login"]),
    ("pip", 1, &["upload"]),
    ("pip3", 1, &["upload"]),
    ("uv", 1, &["publish"]),
    ("poetry", 1, &["publish"]),
    ("flit", 1, &["publish"]),
    ("gem", 1, &["push", "yank", "owner"]),
    ("mvn", 2, &["deploy"]),
    ("gradle", 2, &["publish"]),
    ("helm", 1, &["push"]),
    ("sbt", 2, &["publish"]),
];

const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "ash", "fish", "csh", "tcsh",
];

const WRAPPERS: &[&str] = &[
    "env", "command", "exec", "nohup", "time", "timeout", "nice", "ionice", "stdbuf", "setsid",
    "xargs", "builtin", "taskset", "chrt",
];

const WRAPPER_VALUE_FLAGS: &[(&str, &[&str])] = &[
    ("env", &["-u", "-C", "--unset", "--chdir"]),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("nice", &["-n", "--adjustment"]),
    ("ionice", &["-c", "-n", "-p"]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("xargs", &["-n", "-I", "-P", "-d", "-a", "-E", "-L", "-s"]),
    ("taskset", &["-c"]),
];

const KEYWORDS: &[&str] = &[
    "{", "}", "!", "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "time",
    "function", "select",
];

const ASSIGNMENT_BUILTINS: &[&str] = &["export", "declare", "readonly", "local", "typeset"];

const INTERPRETERS: &[(&str, &[&str])] = &[
    ("python", &["-c"]),
    ("perl", &["e", "E"]),
    ("ruby", &["e"]),
    ("node", &["e", "p"]),
    ("php", &["-r"]),
];

const INTERPRETER_DANGER_MARKERS: &[&str] = &[
    "system",
    "exec",
    "popen",
    "spawn",
    "subprocess",
    "socket",
    "urllib",
    "http",
    "requests",
    "rmtree",
    "child_process",
    "getattr",
    "__",
    "importlib",
    "ctypes",
    "eval",
    "compile(",
    "fork",
    "pty",
    "shell_exec",
    "passthru",
    "qx",
    "`",
    "%x",
    "require(",
    "process.",
    "open(",
    "remove(",
    "unlink",
    "rmdir",
    "globals",
    "vars(",
];

const MAX_NESTING: usize = 4;

const SECRET_MARKERS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".netrc",
    ".git-credentials",
    "id_rsa",
    "id_ed25519",
    "/etc/shadow",
    "/proc/self/environ",
    "/environ",
    "docker.sock",
];

const PATH_WRITERS: &[&str] = &[
    "cp", "mv", "ln", "install", "touch", "mkdir", "tee", "truncate", "rmdir", "unlink", "chmod",
    "chown", "chgrp", "patch",
];

const PROTECTED_BRANCHES: &[&str] = &[
    "main",
    "master",
    "trunk",
    "develop",
    "production",
    "release",
];

const BLOCKED_GIT_CONFIG: &[&str] = &[
    "remote.",
    "url.",
    "core.hookspath",
    "core.sshcommand",
    "core.fsmonitor",
    "core.pager",
    "core.editor",
    "core.askpass",
    "alias.",
    "credential.",
    "include",
    "http.",
    "protocol.",
];

const GH_BLOCKED_TOPLEVEL: &[&str] = &[
    "release",
    "auth",
    "secret",
    "ssh-key",
    "gpg-key",
    "extension",
    "alias",
    "ruleset",
];

const GH_BLOCKED_REPO: &[&str] = &[
    "delete",
    "archive",
    "unarchive",
    "rename",
    "edit",
    "create",
    "transfer",
    "deploy-key",
];

const DOCKER_BINARIES: &[&str] = &["docker", "podman", "nerdctl"];

const DOCKER_SYSTEM_MOUNTS: &[&str] = &[
    "/etc", "/root", "/var", "/usr", "/bin", "/sbin", "/lib", "/boot", "/proc", "/sys", "/dev",
    "/run",
];

type Vars = HashMap<String, Option<String>>;

#[derive(Clone, Debug)]
struct Word {
    text: String,
    dynamic: bool,
    glob: bool,
}

struct Redirect {
    op: String,
    target: Word,
}

struct Simple {
    words: Vec<Word>,
    redirects: Vec<Redirect>,
    vars: Vars,
}

pub(super) fn inspect(tool: &str, args: &Value) -> Result<(), String> {
    match tool {
        "run_command" => inspect_command(args),
        "write_file" => inspect_write_path(args),
        _ => Ok(()),
    }
}

fn inspect_write_path(args: &Value) -> Result<(), String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing or non-string `path`".to_string())?;
    if path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':') {
        return Err(format!("`{path}` is outside the workspace"));
    }
    for component in path.split(['/', '\\']) {
        if component == ".." {
            return Err(format!("`{path}` escapes the workspace"));
        }
        if component.eq_ignore_ascii_case(".git") {
            return Err(format!("`{path}` writes into git internals"));
        }
    }
    Ok(())
}

fn inspect_command(args: &Value) -> Result<(), String> {
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing or non-string `command`".to_string())?;
    inspect_script(command, 0, Vars::new())
}

fn inspect_script(script: &str, depth: usize, vars: Vars) -> Result<(), String> {
    check_secret(&script.to_lowercase())?;
    let simples = parse_script(script, depth, vars)?;
    let mut written = HashSet::new();
    for simple in &simples {
        inspect_simple(simple, depth, &mut written)?;
    }
    Ok(())
}

fn check_secret(lowered: &str) -> Result<(), String> {
    match SECRET_MARKERS.iter().find(|m| lowered.contains(**m)) {
        Some(marker) => Err(format!("touches credential material (`{marker}`)")),
        None => Ok(()),
    }
}

fn parse_script(src: &str, depth: usize, vars: Vars) -> Result<Vec<Simple>, String> {
    if depth > MAX_NESTING {
        return Err("nests shells too deeply to review".to_string());
    }
    let mut parser = Parser {
        chars: src.chars().collect(),
        pos: 0,
        depth,
        vars,
        out: Vec::new(),
        words: Vec::new(),
        redirects: Vec::new(),
        cur: String::new(),
        started: false,
        dynamic: false,
        glob: false,
        pending: None,
        heredocs: Vec::new(),
    };
    parser.run()?;
    Ok(parser.out)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
    vars: Vars,
    out: Vec<Simple>,
    words: Vec<Word>,
    redirects: Vec<Redirect>,
    cur: String,
    started: bool,
    dynamic: bool,
    glob: bool,
    pending: Option<String>,
    heredocs: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn run(&mut self) -> Result<(), String> {
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' => {
                    self.finish_word();
                    self.pos += 1;
                }
                '\n' => {
                    self.end_command();
                    self.pos += 1;
                    self.skip_heredocs()?;
                }
                '#' if !self.started => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                ';' => {
                    self.end_command();
                    self.pos += 1;
                }
                '&' if self.peek_at(1) == Some('>') => self.redirect()?,
                '&' => {
                    self.end_command();
                    self.pos += 1;
                }
                '|' => {
                    self.end_command();
                    self.pos += 1;
                }
                '(' | ')' => {
                    self.end_command();
                    self.pos += 1;
                }
                '<' | '>' => self.redirect()?,
                _ => self.word_char(c)?,
            }
        }
        self.end_command();
        Ok(())
    }

    fn finish_word(&mut self) {
        if !self.started {
            return;
        }
        let word = Word {
            text: std::mem::take(&mut self.cur),
            dynamic: self.dynamic,
            glob: self.glob,
        };
        self.started = false;
        self.dynamic = false;
        self.glob = false;
        match self.pending.take() {
            Some(op) if op.starts_with("<<") && op != "<<<" => self.heredocs.push(word.text),
            Some(op) => self.redirects.push(Redirect { op, target: word }),
            None => self.words.push(word),
        }
    }

    fn end_command(&mut self) {
        self.finish_word();
        self.pending = None;
        let words = std::mem::take(&mut self.words);
        let redirects = std::mem::take(&mut self.redirects);
        if words.is_empty() && redirects.is_empty() {
            return;
        }
        let start = usize::from(
            words
                .first()
                .is_some_and(|w| ASSIGNMENT_BUILTINS.contains(&w.text.as_str())),
        );
        let assignments_only =
            words.len() > start && words[start..].iter().all(|w| is_assignment(&w.text));
        if assignments_only {
            for word in &words[start..] {
                if let Some((name, value)) = word.text.split_once('=') {
                    let value = (!word.dynamic).then(|| value.to_string());
                    self.vars.insert(name.to_string(), value);
                }
            }
            if redirects.is_empty() {
                return;
            }
            self.out.push(Simple {
                words: Vec::new(),
                redirects,
                vars: self.vars.clone(),
            });
            return;
        }
        self.out.push(Simple {
            words,
            redirects,
            vars: self.vars.clone(),
        });
    }

    fn skip_heredocs(&mut self) -> Result<(), String> {
        for delimiter in std::mem::take(&mut self.heredocs) {
            while self.pos < self.chars.len() {
                let start = self.pos;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                let line: String = self.chars[start..self.pos].iter().collect();
                if self.pos < self.chars.len() {
                    self.pos += 1;
                }
                if line.trim_start_matches('\t') == delimiter {
                    break;
                }
                if line.contains("$(") || line.contains('`') {
                    return Err("heredoc body contains a command substitution".to_string());
                }
            }
        }
        Ok(())
    }

    fn redirect(&mut self) -> Result<(), String> {
        let first = self.chars[self.pos];
        if first != '&' && self.peek_at(1) == Some('(') {
            self.pos += 1;
            let body = self.read_balanced('(', ')')?;
            self.substitute(&body)?;
            self.started = true;
            self.dynamic = true;
            return Ok(());
        }
        if self.started
            && !self.dynamic
            && !self.cur.is_empty()
            && self.cur.chars().all(|c| c.is_ascii_digit())
        {
            self.cur.clear();
            self.started = false;
        } else {
            self.finish_word();
        }
        let mut op = String::new();
        if first == '&' {
            op.push('&');
            self.pos += 1;
        }
        while let Some(ch) = self.peek() {
            if (ch == '<' || ch == '>') && op.len() < 4 {
                op.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        match self.peek() {
            Some('&') | Some('|') if !op.starts_with('&') => {
                op.push(self.chars[self.pos]);
                self.pos += 1;
            }
            Some('-') if op == "<<" => {
                op.push('-');
                self.pos += 1;
            }
            _ => {}
        }
        self.pending = Some(op);
        Ok(())
    }

    fn word_char(&mut self, c: char) -> Result<(), String> {
        match c {
            '\\' => {
                self.pos += 1;
                match self.peek() {
                    Some('\n') => self.pos += 1,
                    Some(next) => {
                        self.cur.push(next);
                        self.started = true;
                        self.pos += 1;
                    }
                    None => {
                        self.cur.push('\\');
                        self.started = true;
                    }
                }
            }
            '\'' => {
                self.pos += 1;
                self.started = true;
                loop {
                    match self.peek() {
                        None => return Err("unterminated quote".to_string()),
                        Some('\'') => {
                            self.pos += 1;
                            break;
                        }
                        Some(ch) => {
                            self.cur.push(ch);
                            self.pos += 1;
                        }
                    }
                }
            }
            '"' => self.double_quote()?,
            '$' => self.dollar(false)?,
            '`' => self.backtick()?,
            '*' | '?' | '{' => {
                self.cur.push(c);
                self.started = true;
                self.glob = true;
                self.pos += 1;
            }
            _ => {
                self.cur.push(c);
                self.started = true;
                self.pos += 1;
            }
        }
        Ok(())
    }

    fn double_quote(&mut self) -> Result<(), String> {
        self.pos += 1;
        self.started = true;
        loop {
            match self.peek() {
                None => return Err("unterminated quote".to_string()),
                Some('"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\n') => self.pos += 1,
                        Some(next @ ('"' | '\\' | '$' | '`')) => {
                            self.cur.push(next);
                            self.pos += 1;
                        }
                        _ => self.cur.push('\\'),
                    }
                }
                Some('$') => self.dollar(true)?,
                Some('`') => self.backtick()?,
                Some(ch) => {
                    self.cur.push(ch);
                    self.pos += 1;
                }
            }
        }
    }

    fn dollar(&mut self, quoted: bool) -> Result<(), String> {
        self.pos += 1;
        match self.peek() {
            Some('(') => {
                let body = self.read_balanced('(', ')')?;
                self.substitute(&body)?;
                self.started = true;
                self.dynamic = true;
            }
            Some('{') => {
                let body = self.read_balanced('{', '}')?;
                let name: String = body
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && name == body {
                    self.expand(&name, quoted);
                } else {
                    if body.contains("$(") || body.contains('`') {
                        self.substitute(&body)?;
                    }
                    self.cur.push_str("${");
                    self.cur.push_str(&body);
                    self.cur.push('}');
                    self.started = true;
                    self.dynamic = true;
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        name.push(ch);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                self.expand(&name, quoted);
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                self.cur.push('$');
                self.cur.push(c);
                self.pos += 1;
                self.started = true;
                self.dynamic = true;
            }
            _ => {
                self.cur.push('$');
                self.started = true;
            }
        }
        Ok(())
    }

    fn expand(&mut self, name: &str, quoted: bool) {
        match self.vars.get(name).cloned() {
            Some(Some(value)) => {
                if quoted {
                    self.cur.push_str(&value);
                    self.started = true;
                } else {
                    for ch in value.chars() {
                        if ch.is_whitespace() {
                            self.finish_word();
                        } else {
                            self.cur.push(ch);
                            self.started = true;
                        }
                    }
                }
            }
            _ => {
                self.cur.push('$');
                self.cur.push_str(name);
                self.started = true;
                self.dynamic = true;
            }
        }
    }

    fn backtick(&mut self) -> Result<(), String> {
        self.pos += 1;
        let mut body = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated command substitution".to_string()),
                Some('`') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') if matches!(self.peek_at(1), Some('`' | '\\')) => {
                    body.push(self.chars[self.pos + 1]);
                    self.pos += 2;
                }
                Some(ch) => {
                    body.push(ch);
                    self.pos += 1;
                }
            }
        }
        self.substitute(&body)?;
        self.started = true;
        self.dynamic = true;
        Ok(())
    }

    fn read_balanced(&mut self, open: char, close: char) -> Result<String, String> {
        self.pos += 1;
        let start = self.pos;
        let mut level = 1usize;
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.pos += 1,
                '\'' => {
                    self.pos += 1;
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.pos += 1;
                    }
                }
                '"' => {
                    self.pos += 1;
                    while let Some(q) = self.peek() {
                        if q == '\\' {
                            self.pos += 1;
                        } else if q == '"' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                c if c == open => level += 1,
                c if c == close => {
                    level -= 1;
                    if level == 0 {
                        let body: String = self.chars[start..self.pos].iter().collect();
                        self.pos += 1;
                        return Ok(body);
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        Err("unbalanced substitution".to_string())
    }

    fn substitute(&mut self, body: &str) -> Result<(), String> {
        let nested = parse_script(body, self.depth + 1, self.vars.clone())?;
        self.out.extend(nested);
        Ok(())
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn lower(text: &str) -> String {
    text.to_lowercase()
}

fn basename(word: &str) -> String {
    lower(word.rsplit('/').next().unwrap_or(word))
}

fn normalise(text: &str) -> String {
    let lowered = lower(text);
    let absolute = lowered.starts_with('/');
    let parts: Vec<&str> = lowered
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

fn tmp_path(normalised: &str) -> bool {
    ["/tmp/", "/var/tmp/"]
        .iter()
        .any(|prefix| normalised.len() > prefix.len() && normalised.starts_with(prefix))
}

fn path_escape(text: &str) -> Option<&'static str> {
    let normalised = normalise(text);
    if normalised.starts_with('~') {
        return Some("targets the home directory");
    }
    if normalised.starts_with('$') {
        return Some("expands a variable");
    }
    if normalised.split('/').any(|c| c == "..") {
        return Some("escapes the workspace");
    }
    if normalised.split('/').any(|c| c == ".git") {
        return Some("touches git internals");
    }
    if normalised.starts_with('/') && !tmp_path(&normalised) {
        return Some("is outside the workspace");
    }
    None
}

fn is_flag(word: &Word) -> bool {
    word.text.starts_with('-') && word.text.len() > 1
}

fn positionals(args: &[Word]) -> Vec<&Word> {
    let mut found = Vec::new();
    let mut flags_done = false;
    for arg in args {
        if !flags_done && arg.text == "--" {
            flags_done = true;
        } else if flags_done || !is_flag(arg) {
            found.push(arg);
        }
    }
    found
}

fn strip_prefixes(words: &[Word]) -> Result<(&[Word], bool), String> {
    let mut rest = words;
    let mut xargs = false;
    loop {
        let Some(first) = rest.first() else {
            return Ok((rest, xargs));
        };
        if KEYWORDS.contains(&first.text.as_str()) && !first.dynamic {
            rest = &rest[1..];
            continue;
        }
        if is_assignment(&first.text) {
            rest = &rest[1..];
            continue;
        }
        let name = basename(&first.text);
        if first.dynamic || !WRAPPERS.contains(&name.as_str()) {
            return Ok((rest, xargs));
        }
        xargs |= name == "xargs";
        let value_flags: &[&str] = WRAPPER_VALUE_FLAGS
            .iter()
            .find(|(wrapper, _)| *wrapper == name)
            .map_or(&[], |(_, flags)| flags);
        rest = &rest[1..];
        while let Some(next) = rest.first() {
            let text = next.text.as_str();
            if name == "command" && (text == "-v" || text == "-V") {
                return Ok((&[], xargs));
            }
            if name == "env" && (text == "-S" || text.starts_with("--split-string")) {
                return Err("`env -S` runs a string as a command line".to_string());
            }
            if text.starts_with('-') {
                rest = &rest[1..];
                if value_flags.contains(&text) && !rest.is_empty() {
                    rest = &rest[1..];
                }
            } else if (name == "env" && is_assignment(text))
                || (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || c == '.'))
            {
                rest = &rest[1..];
            } else {
                break;
            }
        }
        if rest.is_empty() && name == "env" {
            return Err("`env` dumps the environment".to_string());
        }
    }
}

fn inspect_simple(
    simple: &Simple,
    depth: usize,
    written: &mut HashSet<String>,
) -> Result<(), String> {
    for redirect in &simple.redirects {
        inspect_redirect(redirect, written)?;
    }
    inspect_words(&simple.words, &simple.vars, depth, written)
}

fn inspect_redirect(redirect: &Redirect, written: &mut HashSet<String>) -> Result<(), String> {
    let op = redirect.op.as_str();
    let target = &redirect.target;
    let text = lower(&target.text);
    check_secret(&text)?;
    if text.starts_with("/dev/tcp/") || text.starts_with("/dev/udp/") {
        return Err("redirects to a network device".to_string());
    }
    if op == "<<<" || op == "<" || op == "<&" {
        return Ok(());
    }
    if op.ends_with('&') && (text == "-" || text.chars().all(|c| c.is_ascii_digit())) {
        return Ok(());
    }
    if target.dynamic {
        return Err("redirects output to a path that cannot be resolved".to_string());
    }
    if ["/dev/null", "/dev/stdout", "/dev/stderr"].contains(&text.as_str()) {
        return Ok(());
    }
    if let Some(why) = path_escape(&target.text) {
        return Err(format!("redirects output to `{}` which {why}", target.text));
    }
    written.insert(normalise(&target.text));
    Ok(())
}

fn inspect_words(
    words: &[Word],
    vars: &Vars,
    depth: usize,
    written: &mut HashSet<String>,
) -> Result<(), String> {
    let (words, xargs) = strip_prefixes(words)?;
    let Some(first) = words.first() else {
        return Ok(());
    };
    for word in words {
        check_secret(&lower(&word.text))?;
    }
    if first.dynamic || first.glob {
        return Err("command name cannot be resolved statically".to_string());
    }
    if written.contains(&normalise(&first.text)) {
        return Err("executes a file written earlier in the same command".to_string());
    }
    let program = basename(&first.text);
    let program = program.as_str();
    let args = &words[1..];
    if NETWORK_BINARIES.contains(&program) {
        return Err(format!("`{program}` can send data off the machine"));
    }
    if PRIVILEGE_BINARIES.contains(&program) {
        return Err(format!("`{program}` escalates privileges"));
    }
    if DISK_BINARIES.contains(&program) || program.starts_with("mkfs") {
        return Err(format!("`{program}` is destructive to storage"));
    }
    if SYSTEM_BINARIES.contains(&program) {
        return Err(format!("`{program}` changes system state"));
    }
    if PUBLISH_BINARIES.contains(&program) {
        return Err(format!(
            "`{program}` publishes or deploys outside the workspace"
        ));
    }
    if program == "printenv" {
        return Err("`printenv` dumps the environment".to_string());
    }
    if xargs && ["rm", "chmod", "chown", "mv", "shred"].contains(&program) {
        return Err(format!(
            "`xargs {program}` acts on targets that cannot be reviewed"
        ));
    }
    if SHELLS.contains(&program) {
        return inspect_shell(args, vars, depth);
    }
    match program {
        "eval" => return inspect_eval(args, vars, depth),
        "source" | "." => return Err("sources a script that cannot be reviewed".to_string()),
        "trap" => return inspect_trap(args, vars, depth),
        "rm" => return inspect_rm(args),
        "git" => return inspect_git(args),
        "gh" => return inspect_gh(args),
        "find" => return inspect_find(args, vars, depth, written),
        "kill" => return inspect_kill(args),
        _ => {}
    }
    if DOCKER_BINARIES.contains(&program) {
        inspect_docker(args)?;
    }
    if PATH_WRITERS.contains(&program) || (program == "sed" && has_in_place(args)) {
        inspect_path_writer(program, args, written)?;
    }
    inspect_publish(program, args)?;
    inspect_interpreter(program, args, written)
}

fn inspect_shell(args: &[Word], vars: &Vars, depth: usize) -> Result<(), String> {
    let mut i = 0;
    let mut code = None;
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        if text == "--" {
            break;
        }
        if ["-o", "+o", "-O", "+O"].contains(&text) {
            i += 2;
            continue;
        }
        if text.starts_with("--") || text.starts_with('+') {
            i += 1;
            continue;
        }
        if text.starts_with('-') && text.len() > 1 {
            if text[1..].contains('c') {
                code = args.get(i + 1);
                break;
            }
            i += 1;
            continue;
        }
        break;
    }
    let Some(code) = code else {
        return Err("runs a script or stdin through a shell".to_string());
    };
    if code.dynamic {
        return Err("runs a shell command string that cannot be resolved".to_string());
    }
    inspect_script(&code.text, depth + 1, vars.clone())
}

fn inspect_eval(args: &[Word], vars: &Vars, depth: usize) -> Result<(), String> {
    if args.iter().any(|a| a.dynamic) {
        return Err("`eval` of a value that cannot be resolved".to_string());
    }
    let joined: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
    inspect_script(&joined.join(" "), depth + 1, vars.clone())
}

fn inspect_trap(args: &[Word], vars: &Vars, depth: usize) -> Result<(), String> {
    let Some(handler) = positionals(args).into_iter().next() else {
        return Ok(());
    };
    if handler.dynamic {
        return Err("`trap` handler cannot be resolved".to_string());
    }
    inspect_script(&handler.text, depth + 1, vars.clone())
}

fn inspect_rm(args: &[Word]) -> Result<(), String> {
    let flags: Vec<&str> = args
        .iter()
        .take_while(|a| a.text != "--")
        .filter(|a| is_flag(a))
        .map(|a| a.text.as_str())
        .collect();
    if flags.contains(&"--no-preserve-root") {
        return Err("`rm --no-preserve-root` is destructive".to_string());
    }
    let recursive = flags
        .iter()
        .any(|f| *f == "--recursive" || (!f.starts_with("--") && f.contains(['r', 'R'])));
    for target in positionals(args) {
        if let Some(why) = path_escape(&target.text) {
            return Err(format!("`rm` of `{}` {why}", target.text));
        }
        if recursive {
            let normalised = normalise(&target.text);
            if target.dynamic || [".", "..", "*", ".*"].contains(&normalised.as_str()) {
                return Err(format!(
                    "recursive `rm` of `{}` is destructive",
                    target.text
                ));
            }
        }
    }
    Ok(())
}

fn inspect_kill(args: &[Word]) -> Result<(), String> {
    if args.iter().any(|a| a.text == "-1" || a.text == "0") {
        return Err("`kill` would signal every process".to_string());
    }
    Ok(())
}

fn inspect_find(
    args: &[Word],
    vars: &Vars,
    depth: usize,
    written: &mut HashSet<String>,
) -> Result<(), String> {
    let roots: Vec<&Word> = args
        .iter()
        .take_while(|a| !is_flag(a) && a.text != "!" && a.text != "(")
        .collect();
    let escaping = roots
        .iter()
        .any(|r| r.dynamic || path_escape(&r.text).is_some());
    let deletes = args.iter().any(|a| a.text == "-delete");
    let mut executes = false;
    let mut i = 0;
    while i < args.len() {
        if ["-exec", "-execdir", "-ok", "-okdir"].contains(&args[i].text.as_str()) {
            executes = true;
            let inner: Vec<Word> = args[i + 1..]
                .iter()
                .take_while(|a| a.text != ";" && a.text != "+")
                .cloned()
                .collect();
            i += inner.len() + 1;
            inspect_words(&inner, vars, depth, written)?;
        }
        i += 1;
    }
    if escaping && (deletes || executes) {
        return Err("`find` deletes or executes outside the workspace".to_string());
    }
    Ok(())
}

fn has_in_place(args: &[Word]) -> bool {
    args.iter()
        .any(|a| a.text.starts_with("-i") || a.text == "--in-place")
}

fn inspect_path_writer(
    program: &str,
    args: &[Word],
    written: &mut HashSet<String>,
) -> Result<(), String> {
    let mut targets = positionals(args);
    if program == "sed" {
        targets = targets.last().copied().into_iter().collect();
    }
    for target in &targets {
        if let Some(why) = path_escape(&target.text) {
            return Err(format!("`{program}` writes `{}` which {why}", target.text));
        }
    }
    let recorded: Vec<&&Word> = match program {
        "tee" => targets.iter().collect(),
        "cp" | "mv" | "install" => targets.last().into_iter().collect(),
        _ => Vec::new(),
    };
    for target in recorded {
        written.insert(normalise(&target.text));
    }
    Ok(())
}

fn inspect_publish(program: &str, args: &[Word]) -> Result<(), String> {
    let Some((_, window, subcommands)) = PUBLISH_SUBCOMMANDS
        .iter()
        .find(|(name, _, _)| *name == program)
    else {
        return Ok(());
    };
    let leading = args
        .iter()
        .take_while(|a| a.text != "--")
        .filter(|a| !is_flag(a) && !a.text.starts_with('+'))
        .take(*window);
    for arg in leading {
        if subcommands.contains(&lower(&arg.text).as_str()) {
            return Err(format!(
                "`{program} {}` publishes outside the workspace",
                arg.text
            ));
        }
    }
    Ok(())
}

fn inspect_docker(args: &[Word]) -> Result<(), String> {
    let sub = positionals(args).first().map(|a| lower(&a.text));
    if matches!(sub.as_deref(), Some("push" | "login" | "logout")) {
        return Err("publishes images or handles registry credentials".to_string());
    }
    let mut mounts: Vec<String> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let text = lower(&arg.text);
        for risky in [
            "--privileged",
            "--cap-add",
            "--pid=host",
            "--userns=host",
            "--device",
            "unconfined",
            "docker.sock",
        ] {
            if text.contains(risky) {
                return Err(format!("container option `{risky}` weakens confinement"));
            }
        }
        if text == "-v" || text == "--volume" || text == "--mount" {
            if let Some(next) = args.get(i + 1) {
                mounts.push(lower(&next.text));
            }
        } else if let Some(value) = text
            .strip_prefix("--volume=")
            .or_else(|| text.strip_prefix("--mount="))
        {
            mounts.push(value.to_string());
        }
    }
    for mount in mounts {
        let source = mount
            .split(',')
            .find_map(|part| {
                part.strip_prefix("source=")
                    .or_else(|| part.strip_prefix("src="))
            })
            .unwrap_or_else(|| mount.split(':').next().unwrap_or(""));
        let source = normalise(source);
        let system = source == "/"
            || source == "/home"
            || DOCKER_SYSTEM_MOUNTS
                .iter()
                .any(|dir| source == *dir || source.starts_with(&format!("{dir}/")));
        if system {
            return Err(format!("mounts `{source}` into a container"));
        }
    }
    Ok(())
}

fn inspect_interpreter(
    program: &str,
    args: &[Word],
    written: &HashSet<String>,
) -> Result<(), String> {
    if ["awk", "gawk", "mawk", "nawk"].contains(&program) {
        if args
            .iter()
            .any(|a| lower(&a.text).contains("system(") || lower(&a.text).contains("popen"))
        {
            return Err(format!("`{program}` program spawns processes"));
        }
        return Ok(());
    }
    let Some((family, flags)) = INTERPRETERS
        .iter()
        .find(|(name, _)| program.starts_with(name))
    else {
        return Ok(());
    };
    let texts: Vec<String> = args.iter().map(|a| lower(&a.text)).collect();
    if texts.iter().any(|t| t == "twine") {
        return Err("publishes packages".to_string());
    }
    if texts.iter().any(|t| t.ends_with("setup.py")) && texts.iter().any(|t| t == "upload") {
        return Err("publishes packages".to_string());
    }
    let mut script = None;
    let mut module = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        if text == "-m" && *family == "python" {
            module = true;
            if texts.get(i + 1).is_some_and(|m| {
                m.starts_with("http") || m.starts_with("smtp") || m.starts_with("ftp")
            }) {
                return Err("`python -m` starts a network service".to_string());
            }
            break;
        }
        if ["--version", "-v", "-V", "--help", "-h"].contains(&text) {
            return Ok(());
        }
        let eval_code = if is_eval_flag(family, flags, text) {
            let attached = (*family == "python" || *family == "php") && text.len() > 2;
            if attached {
                Some(text[2..].to_string())
            } else {
                match args.get(i + 1) {
                    Some(code) if code.dynamic => {
                        return Err("evaluates code that cannot be resolved".to_string())
                    }
                    Some(code) => Some(code.text.clone()),
                    None => None,
                }
            }
        } else if *family == "node" && (text == "--eval" || text == "--print") {
            args.get(i + 1).map(|code| code.text.clone())
        } else {
            None
        };
        if let Some(code) = eval_code {
            let code = lower(&code);
            return match INTERPRETER_DANGER_MARKERS.iter().find(|m| code.contains(**m)) {
                Some(marker) => Err(format!(
                    "`{program}` evaluates code that can spawn processes or open sockets (`{marker}`)"
                )),
                None => Ok(()),
            };
        }
        if !text.starts_with('-') || text == "-" {
            script = Some(arg);
            break;
        }
        i += 1;
    }
    match script {
        Some(path) if written.contains(&normalise(&path.text)) => {
            Err("runs a file written earlier in the same command".to_string())
        }
        Some(path) if path.text == "-" => Err(format!("`{program}` reads code from stdin")),
        Some(_) => Ok(()),
        None if module => Ok(()),
        None => Err(format!("`{program}` reads code from stdin")),
    }
}

fn is_eval_flag(family: &str, flags: &[&str], text: &str) -> bool {
    match family {
        "python" | "php" => text.starts_with(flags[0]) && !text.starts_with("--"),
        _ => {
            text.starts_with('-')
                && !text.starts_with("--")
                && text.len() > 1
                && text[1..].chars().all(|c| c.is_ascii_alphabetic())
                && text
                    .chars()
                    .last()
                    .is_some_and(|c| flags.contains(&c.to_string().as_str()))
        }
    }
}

fn inspect_git(args: &[Word]) -> Result<(), String> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        if text == "-c" {
            if let Some(kv) = args.get(i + 1) {
                check_git_config_key(kv.text.split('=').next().unwrap_or(""))?;
            }
            i += 2;
        } else if [
            "-C",
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--exec-path",
        ]
        .contains(&text)
        {
            i += 2;
        } else if text.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let Some(sub) = args.get(i) else {
        return Ok(());
    };
    let rest = &args[i + 1..];
    match sub.text.as_str() {
        "push" => inspect_git_push(rest),
        "remote" => {
            let first = positionals(rest).first().map(|a| lower(&a.text));
            match first.as_deref() {
                Some(
                    "add" | "set-url" | "rename" | "remove" | "rm" | "set-head" | "set-branches",
                ) => Err("changing git remotes can redirect pushes".to_string()),
                _ => Ok(()),
            }
        }
        "config" => {
            if rest.iter().any(|a| {
                [
                    "--global",
                    "--system",
                    "--file",
                    "-f",
                    "--blob",
                    "--worktree",
                ]
                .contains(&a.text.as_str())
            }) {
                return Err("`git config` outside the repository".to_string());
            }
            match positionals(rest).first() {
                Some(key) => check_git_config_key(&key.text),
                None => Ok(()),
            }
        }
        "credential" | "credential-store" | "credential-cache" => {
            Err("`git credential` handles secrets".to_string())
        }
        _ => Ok(()),
    }
}

fn check_git_config_key(key: &str) -> Result<(), String> {
    let key = lower(key);
    match BLOCKED_GIT_CONFIG.iter().find(|p| key.starts_with(**p)) {
        Some(_) => Err(format!("git config `{key}` can redirect or execute")),
        None => Ok(()),
    }
}

fn inspect_git_push(args: &[Word]) -> Result<(), String> {
    let mut targets: Vec<&Word> = Vec::new();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        if text == "--" {
            targets.extend(&args[i + 1..]);
            break;
        }
        if let Some(long) = text.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            if name.starts_with("force")
                || ["mirror", "delete", "prune", "all", "tags", "follow-tags"].contains(&name)
            {
                return Err(format!(
                    "`git push --{name}` can rewrite or publish shared refs"
                ));
            }
            if ["repo", "receive-pack", "exec", "push-option"].contains(&name)
                && !long.contains('=')
            {
                i += 1;
            }
        } else if text.starts_with('-') && text.len() > 1 {
            let cluster = &text[1..];
            if cluster.contains(['f', 'd']) {
                return Err("force or delete push rewrites shared history".to_string());
            }
            if cluster == "o" {
                i += 1;
            }
        } else {
            targets.push(arg);
        }
        i += 1;
    }
    let Some(remote) = targets.first() else {
        return Ok(());
    };
    let named = !remote.dynamic
        && !remote.text.is_empty()
        && remote
            .text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !named {
        return Err("pushes to something other than a configured remote name".to_string());
    }
    for spec in &targets[1..] {
        if spec.dynamic {
            return Err("pushes a refspec that cannot be resolved".to_string());
        }
        let spec = lower(&spec.text);
        if spec.starts_with('+') || spec.starts_with(':') {
            return Err("force or delete refspec rewrites shared history".to_string());
        }
        let destination = spec.rsplit(':').next().unwrap_or(&spec);
        let branch = destination
            .strip_prefix("refs/heads/")
            .unwrap_or(destination);
        if PROTECTED_BRANCHES.contains(&branch) {
            return Err(format!("pushing to protected branch `{branch}`"));
        }
        if destination.starts_with("refs/tags/") {
            return Err("pushing tags publishes releases".to_string());
        }
    }
    Ok(())
}

fn inspect_gh(args: &[Word]) -> Result<(), String> {
    let nonflags: Vec<String> = args
        .iter()
        .filter(|a| !is_flag(a) && !a.text.contains('/'))
        .map(|a| lower(&a.text))
        .collect();
    let top = nonflags.first().map(String::as_str).unwrap_or("");
    let sub = nonflags.get(1).map(String::as_str).unwrap_or("");
    if GH_BLOCKED_TOPLEVEL.contains(&top) {
        return Err(format!("`gh {top}` publishes or handles credentials"));
    }
    match (top, sub) {
        ("pr", "merge") => return Err("agents never merge pull requests".to_string()),
        ("pr", "review") if args.iter().any(|a| a.text == "--approve" || a.text == "-a") => {
            return Err("agents never approve pull requests".to_string());
        }
        ("repo", s) if GH_BLOCKED_REPO.contains(&s) => {
            return Err(format!(
                "`gh repo {s}` changes repository existence or settings"
            ));
        }
        ("api", _) => return inspect_gh_api(args),
        _ => {}
    }
    Ok(())
}

fn inspect_gh_api(args: &[Word]) -> Result<(), String> {
    let mut method: Option<String> = None;
    let mut has_fields = false;
    for (i, arg) in args.iter().enumerate() {
        let text = arg.text.as_str();
        if lower(text) == "graphql" {
            return Err("`gh api graphql` can run merge mutations".to_string());
        }
        if text == "-X" || text == "--method" {
            method = args.get(i + 1).map(|m| lower(&m.text));
        } else if let Some(value) = text.strip_prefix("--method=") {
            method = Some(lower(value));
        } else if let Some(value) = text.strip_prefix("-X") {
            method = Some(lower(value));
        } else if ["-f", "-F", "--field", "--raw-field", "--input"].contains(&text)
            || text.starts_with("--field=")
            || text.starts_with("--raw-field=")
            || text.starts_with("--input=")
        {
            has_fields = true;
        }
    }
    match method.as_deref() {
        Some("get" | "head") => Ok(()),
        Some(other) => Err(format!("`gh api -X {other}` mutates remote state")),
        None if has_fields => Err("`gh api` with fields is an implicit POST".to_string()),
        None => Ok(()),
    }
}
