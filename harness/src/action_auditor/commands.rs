use serde_json::Value;

const NETWORK_BINARIES: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "socat", "scp", "sftp", "ssh", "ftp", "telnet", "rsync",
];

const PRIVILEGE_BINARIES: &[&str] = &["sudo", "su", "doas"];

const DISK_BINARIES: &[&str] = &["dd", "shred", "fdisk", "parted", "wipefs"];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

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
    let lowered = command.to_lowercase();
    if let Some(marker) = SECRET_MARKERS.iter().find(|m| lowered.contains(**m)) {
        return Err(format!("touches credential material (`{marker}`)"));
    }
    for (segment, piped) in split_pipeline(&lowered) {
        let words = tokenize(segment);
        let Some(first) = words.first() else {
            continue;
        };
        if piped && SHELLS.contains(&basename(first)) {
            return Err("pipes data into a shell".to_string());
        }
        inspect_words(&words)?;
    }
    Ok(())
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
    word.rsplit('/').next().unwrap_or(word)
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
