use serde_json::Value;

const NETWORK_BINARIES: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "socat", "scp", "sftp", "ssh", "ftp", "telnet", "rsync",
];

const PRIVILEGE_BINARIES: &[&str] = &["sudo", "su", "doas"];

const DISK_BINARIES: &[&str] = &["dd", "shred", "fdisk", "parted", "wipefs"];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

const WRAPPERS: &[&str] = &[
    "env", "command", "exec", "nohup", "time", "timeout", "nice", "ionice", "stdbuf", "setsid",
    "xargs", "builtin",
];

const INTERPRETER_EVAL_FLAGS: &[(&str, &[&str])] = &[
    ("python", &["-c"]),
    ("perl", &["-e", "-E"]),
    ("ruby", &["-e"]),
    ("node", &["-e", "--eval", "-p"]),
    ("php", &["-r"]),
];

const INTERPRETER_DANGER_MARKERS: &[&str] = &[
    "os.system",
    "os.popen",
    "os.exec",
    "os.spawn",
    "subprocess",
    "pty.spawn",
    "child_process",
    "system(",
    "exec(",
    "popen",
    "socket",
    "urllib",
    "http.client",
    "requests.",
    "shutil.rmtree",
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
];

const DANGEROUS_RM_TARGETS: &[&str] = &[
    "/", "/*", "~", "~/", "~/*", "$home", "${home}", "$home/", "$home/*", "*", ".", "..", "./*",
    "../*", "/etc", "/usr", "/bin", "/sbin", "/lib", "/var", "/home", "/root", "/boot", "/opt",
    "/dev", "/sys", "/proc",
];

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
        if component == ".git" {
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
    inspect_script(&command.to_lowercase(), 0)
}

fn inspect_script(script: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_NESTING {
        return Err("nests shells too deeply to review".to_string());
    }
    if let Some(marker) = SECRET_MARKERS.iter().find(|m| script.contains(**m)) {
        return Err(format!("touches credential material (`{marker}`)"));
    }
    for (segment, piped) in split_pipeline(script) {
        let words = tokenize(segment);
        let words = strip_prefixes(&words);
        let Some(first) = words.first() else {
            continue;
        };
        if piped && SHELLS.contains(&basename(first)) {
            return Err("pipes data into a shell".to_string());
        }
        inspect_words(words)?;
    }
    inspect_nested(script, depth)
}

fn inspect_nested(script: &str, depth: usize) -> Result<(), String> {
    let mut offset = 0;
    let mut tokens = Vec::new();
    for word in script.split_whitespace() {
        let start = script[offset..].find(word).map_or(offset, |i| offset + i);
        offset = start + word.len();
        tokens.push((word.trim_matches(['"', '\'']), offset));
    }
    for (i, (token, _)) in tokens.iter().enumerate() {
        let program = basename(token);
        if program == "eval" {
            if let Some((_, end)) = tokens.get(i) {
                inspect_script(unquote(&script[*end..]), depth + 1)?;
            }
            continue;
        }
        let is_shell = SHELLS.contains(&program);
        let eval_flags = INTERPRETER_EVAL_FLAGS
            .iter()
            .find(|(name, _)| program.starts_with(name))
            .map(|(_, flags)| *flags);
        if !is_shell && eval_flags.is_none() {
            continue;
        }
        let flag = tokens[i + 1..]
            .iter()
            .take_while(|(word, _)| word.starts_with('-'))
            .find(|(word, _)| match eval_flags {
                Some(flags) => flags.contains(word),
                None => !word.starts_with("--") && word.contains('c'),
            });
        let Some((_, end)) = flag else {
            continue;
        };
        let code = unquote(&script[*end..]);
        if eval_flags.is_some() {
            if let Some(marker) = INTERPRETER_DANGER_MARKERS
                .iter()
                .find(|m| code.contains(**m))
            {
                return Err(format!(
                    "`{program}` evaluates code that can spawn processes or open sockets (`{marker}`)"
                ));
            }
        }
        inspect_script(code, depth + 1)?;
    }
    Ok(())
}

fn unquote(rest: &str) -> &str {
    let rest = rest.trim();
    match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => {
            let inner = &rest[1..];
            inner.strip_suffix(quote).unwrap_or(inner)
        }
        _ => rest,
    }
}

fn strip_prefixes(words: &[String]) -> &[String] {
    let mut rest = words;
    loop {
        let Some(first) = rest.first() else {
            return rest;
        };
        if is_assignment(first) {
            rest = &rest[1..];
            continue;
        }
        if WRAPPERS.contains(&basename(first)) {
            rest = &rest[1..];
            while let Some(next) = rest.first() {
                let numeric = next.chars().all(|c| c.is_ascii_digit() || c == '.');
                if next.starts_with('-') || is_assignment(next) || numeric {
                    rest = &rest[1..];
                } else {
                    break;
                }
            }
            continue;
        }
        return rest;
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn split_pipeline(command: &str) -> Vec<(&str, bool)> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut piped = false;
    for (i, byte) in command.bytes().enumerate() {
        if matches!(byte, b';' | b'&' | b'|' | b'\n' | b'(' | b')' | b'`') {
            segments.push((&command[start..i], piped));
            piped = byte == b'|';
            start = i + 1;
        }
    }
    segments.push((&command[start..], piped));
    segments
}

fn tokenize(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|word| word.trim_matches(['"', '\'']).to_string())
        .collect()
}

fn basename(word: &str) -> &str {
    word.rsplit('/')
        .next()
        .unwrap_or(word)
        .trim_start_matches('\\')
}

fn inspect_words(words: &[String]) -> Result<(), String> {
    let Some(first) = words.first() else {
        return Ok(());
    };
    let program = basename(first);
    if NETWORK_BINARIES.contains(&program) {
        return Err(format!("`{program}` can send data off the machine"));
    }
    if PRIVILEGE_BINARIES.contains(&program) {
        return Err(format!("`{program}` escalates privileges"));
    }
    if DISK_BINARIES.contains(&program) || program.starts_with("mkfs") {
        return Err(format!("`{program}` is destructive to storage"));
    }
    if program == "printenv" {
        return Err("`printenv` dumps the environment".to_string());
    }
    let args = &words[1..];
    if program == "rm" {
        inspect_rm(args)?;
    }
    if program == "git" {
        inspect_git(args)?;
    }
    Ok(())
}

fn inspect_rm(args: &[String]) -> Result<(), String> {
    let recursive = args.iter().any(|arg| {
        arg == "--recursive"
            || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains(['r', 'R']))
    });
    if !recursive {
        return Ok(());
    }
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .find(|arg| DANGEROUS_RM_TARGETS.contains(&arg.as_str()))
        .map_or(Ok(()), |target| {
            Err(format!("recursive `rm` of `{target}` is destructive"))
        })
}

fn inspect_git(args: &[String]) -> Result<(), String> {
    let pushes = args.iter().any(|arg| arg == "push");
    let forced = args.iter().any(|arg| {
        arg == "--force"
            || arg == "-f"
            || arg.starts_with("--force-with-lease")
            || arg.starts_with("--mirror")
            || arg == "--delete"
    });
    if pushes && forced {
        return Err("force-pushing rewrites shared history".to_string());
    }
    Ok(())
}
