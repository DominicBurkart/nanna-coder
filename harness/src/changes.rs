use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

pub const MAX_FILE_CAPTURE_BYTES: usize = 4_000_000;
pub const MAX_TOTAL_CAPTURE_BYTES: usize = 32_000_000;
pub const PAGE_CONTENT_BYTES: usize = 200_000;
pub const DEFAULT_FILES_PER_PAGE: usize = 50;
pub const MAX_FILES_PER_PAGE: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeStatus::Added => "added",
            ChangeStatus::Modified => "modified",
            ChangeStatus::Deleted => "deleted",
            ChangeStatus::Renamed => "renamed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "added" => Some(ChangeStatus::Added),
            "modified" => Some(ChangeStatus::Modified),
            "deleted" => Some(ChangeStatus::Deleted),
            "renamed" => Some(ChangeStatus::Renamed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapturedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: ChangeStatus,
    pub binary: bool,
    pub content: Option<Vec<u8>>,
    pub size: usize,
    pub capture_truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ChangeCapture {
    pub base_ref: Option<String>,
    pub files: Vec<CapturedFile>,
    pub patch_original_bytes: Option<usize>,
}

fn git(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for var in [
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}

pub fn parse_name_status(raw: &[u8]) -> Vec<(ChangeStatus, String, Option<String>)> {
    let text = String::from_utf8_lossy(raw);
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    let mut entries = Vec::new();
    while let Some(code) = fields.next() {
        match code.chars().next() {
            Some('R') | Some('C') => {
                let (Some(old), Some(new)) = (fields.next(), fields.next()) else {
                    break;
                };
                if code.starts_with('R') {
                    entries.push((
                        ChangeStatus::Renamed,
                        new.to_string(),
                        Some(old.to_string()),
                    ));
                } else {
                    entries.push((ChangeStatus::Added, new.to_string(), None));
                }
            }
            Some(letter) => {
                let Some(path) = fields.next() else { break };
                let status = match letter {
                    'A' => ChangeStatus::Added,
                    'D' => ChangeStatus::Deleted,
                    _ => ChangeStatus::Modified,
                };
                entries.push((status, path.to_string(), None));
            }
            None => break,
        }
    }
    entries
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn without_content(status: ChangeStatus, path: String, old_path: Option<String>) -> CapturedFile {
    CapturedFile {
        path,
        old_path,
        status,
        binary: false,
        content: None,
        size: 0,
        capture_truncated: false,
    }
}

pub fn capture_staged(workspace: &Path) -> Option<ChangeCapture> {
    let listing = git(
        workspace,
        &["diff", "--cached", "--name-status", "-M", "-z", "HEAD"],
    )?;
    let entries = parse_name_status(&listing);
    if entries.is_empty() {
        return None;
    }
    let base_ref = git(workspace, &["rev-parse", "HEAD"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_string());
    let mut total = 0usize;
    let mut files = Vec::with_capacity(entries.len());
    for (status, path, old_path) in entries {
        if status == ChangeStatus::Deleted {
            files.push(without_content(status, path, old_path));
            continue;
        }
        let Some(mut bytes) = git(workspace, &["show", &format!(":{path}")]) else {
            files.push(without_content(status, path, old_path));
            continue;
        };
        let size = bytes.len();
        let binary = is_binary(&bytes);
        let room = MAX_TOTAL_CAPTURE_BYTES.saturating_sub(total);
        let keep = size.min(MAX_FILE_CAPTURE_BYTES).min(room);
        let capture_truncated = keep < size;
        if capture_truncated {
            bytes.truncate(keep);
            if !binary {
                while std::str::from_utf8(&bytes).is_err() {
                    bytes.pop();
                }
            }
        }
        total += bytes.len();
        files.push(CapturedFile {
            path,
            old_path,
            status,
            binary,
            content: Some(bytes),
            size,
            capture_truncated,
        });
    }
    Some(ChangeCapture {
        base_ref,
        files,
        patch_original_bytes: None,
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |byte: u8| B64.iter().position(|c| *c == byte).map(|i| i as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let last = bytes.len() / 4;
    for (index, quad) in bytes.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 || (pad > 0 && index + 1 != last) {
            return None;
        }
        let mut n = 0u32;
        for (slot, byte) in quad.iter().enumerate() {
            let digit = if slot >= 4 - pad { 0 } else { value(*byte)? };
            n = (n << 6) | digit;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn entry(file: &CapturedFile, offset: usize, budget: usize) -> (Value, usize, usize) {
    let mut value = json!({
        "path": file.path,
        "status": file.status.as_str(),
        "binary": file.binary,
        "bytes": file.size,
        "truncated": false,
    });
    if let Some(old) = &file.old_path {
        value["old_path"] = json!(old);
    }
    let Some(content) = &file.content else {
        if file.status != ChangeStatus::Deleted {
            value["content_unavailable"] = json!(true);
        }
        return (value, 0, 0);
    };
    let mut start = offset.min(content.len());
    if !file.binary {
        while start < content.len() && (content[start] & 0xC0) == 0x80 {
            start += 1;
        }
    }
    let mut end = content.len().min(start.saturating_add(budget));
    if !file.binary && end < content.len() {
        while end > start && (content[end] & 0xC0) == 0x80 {
            end -= 1;
        }
    }
    let slice = &content[start..end];
    if file.binary {
        value["content_base64"] = json!(base64_encode(slice));
    } else {
        value["content"] = json!(String::from_utf8_lossy(slice));
    }
    value["content_offset"] = json!(start);
    value["truncated"] = json!(end < file.size);
    (value, start, end)
}

pub fn page(
    capture: &ChangeCapture,
    path: Option<&str>,
    offset: usize,
    limit: Option<usize>,
) -> Result<Value, String> {
    let patch_truncated = capture.patch_original_bytes.is_some();
    if let Some(path) = path {
        let file = capture
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| format!("No changed file `{path}` in this task"))?;
        let budget = limit
            .unwrap_or(PAGE_CONTENT_BYTES)
            .clamp(1, PAGE_CONTENT_BYTES);
        let (value, _, end) = entry(file, offset, budget);
        let held = file.content.as_ref().map_or(0, Vec::len);
        return Ok(json!({
            "base_ref": capture.base_ref,
            "files": [value],
            "next_offset": (end < held).then_some(end),
            "total_files": capture.files.len(),
            "patch_truncated": patch_truncated,
        }));
    }
    let per_page = limit
        .unwrap_or(DEFAULT_FILES_PER_PAGE)
        .clamp(1, MAX_FILES_PER_PAGE);
    let mut remaining = PAGE_CONTENT_BYTES;
    let mut files = Vec::new();
    let mut index = offset;
    while index < capture.files.len() && files.len() < per_page {
        let (value, start, end) = entry(&capture.files[index], 0, remaining);
        remaining = remaining.saturating_sub(end - start);
        files.push(value);
        index += 1;
    }
    Ok(json!({
        "base_ref": capture.base_ref,
        "files": files,
        "next_offset": (index < capture.files.len()).then_some(index),
        "total_files": capture.files.len(),
        "patch_truncated": patch_truncated,
        "note": "A file with truncated=true was cut at the page budget or the capture limit; fetch the rest with path and offset (a byte offset into that file).",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decode_inverts_encode_for_every_padding_length() {
        for len in 0..=40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)), Some(bytes), "{len}");
        }
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(base64_decode(&base64_encode(&all)), Some(all));
    }

    #[test]
    fn base64_decode_rejects_malformed_input() {
        for bad in ["abc", "ab=c", "a===", "====", "AA==AAAA", "!!!!", "AAA*"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn change_status_parses_what_it_prints() {
        for status in [
            ChangeStatus::Added,
            ChangeStatus::Modified,
            ChangeStatus::Deleted,
            ChangeStatus::Renamed,
        ] {
            assert_eq!(ChangeStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ChangeStatus::parse("copied"), None);
    }

    fn text(path: &str, body: &str) -> CapturedFile {
        CapturedFile {
            path: path.to_string(),
            old_path: None,
            status: ChangeStatus::Added,
            binary: false,
            content: Some(body.as_bytes().to_vec()),
            size: body.len(),
            capture_truncated: false,
        }
    }

    fn capture(files: Vec<CapturedFile>) -> ChangeCapture {
        ChangeCapture {
            base_ref: Some("abc123".to_string()),
            files,
            patch_original_bytes: None,
        }
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected);
        }
    }

    #[test]
    fn name_status_parses_every_change_kind() {
        let raw = b"A\0new.rs\0M\0old.rs\0D\0gone.rs\0R100\0from.rs\0to.rs\0";
        assert_eq!(
            parse_name_status(raw),
            vec![
                (ChangeStatus::Added, "new.rs".to_string(), None),
                (ChangeStatus::Modified, "old.rs".to_string(), None),
                (ChangeStatus::Deleted, "gone.rs".to_string(), None),
                (
                    ChangeStatus::Renamed,
                    "to.rs".to_string(),
                    Some("from.rs".to_string())
                ),
            ]
        );
    }

    #[test]
    fn a_page_reports_text_binary_deleted_and_renamed_files() {
        let mut binary = text("img.bin", "");
        binary.binary = true;
        binary.content = Some(vec![0, 1, 2, 255]);
        binary.size = 4;
        let mut deleted = text("gone.rs", "");
        deleted.status = ChangeStatus::Deleted;
        deleted.content = None;
        deleted.size = 0;
        let mut renamed = text("to.rs", "fn a() {}\n");
        renamed.status = ChangeStatus::Renamed;
        renamed.old_path = Some("from.rs".to_string());
        let page = page(
            &capture(vec![text("a.rs", "hello"), binary, deleted, renamed]),
            None,
            0,
            None,
        )
        .unwrap();
        assert_eq!(page["base_ref"], "abc123");
        assert_eq!(page["next_offset"], Value::Null);
        let files = page["files"].as_array().unwrap();
        assert_eq!(files[0]["content"], "hello");
        assert_eq!(files[0]["status"], "added");
        assert_eq!(files[0]["binary"], false);
        assert_eq!(files[1]["binary"], true);
        assert_eq!(files[1]["content_base64"], "AAEC/w==");
        assert!(files[1].get("content").is_none());
        assert_eq!(files[2]["status"], "deleted");
        assert!(files[2].get("content").is_none());
        assert_eq!(files[3]["status"], "renamed");
        assert_eq!(files[3]["old_path"], "from.rs");
    }

    #[test]
    fn paging_walks_the_file_list_with_next_offset() {
        let files = (0..5).map(|i| text(&format!("f{i}"), "x")).collect();
        let c = capture(files);
        let first = page(&c, None, 0, Some(2)).unwrap();
        assert_eq!(first["files"].as_array().unwrap().len(), 2);
        assert_eq!(first["next_offset"], 2);
        let last = page(&c, None, 4, Some(2)).unwrap();
        assert_eq!(last["files"].as_array().unwrap().len(), 1);
        assert_eq!(last["next_offset"], Value::Null);
        assert_eq!(last["files"][0]["path"], "f4");
    }

    #[test]
    fn a_page_stays_within_the_content_budget_and_flags_truncation() {
        let big = "a".repeat(PAGE_CONTENT_BYTES + 500);
        let c = capture(vec![text("big", &big), text("small", "tail")]);
        let first = page(&c, None, 0, None).unwrap();
        let big_entry = &first["files"][0];
        assert_eq!(big_entry["truncated"], true);
        assert_eq!(big_entry["bytes"], big.len());
        assert_eq!(
            big_entry["content"].as_str().unwrap().len(),
            PAGE_CONTENT_BYTES
        );
        assert_eq!(first["files"][1]["content"], "");
        assert_eq!(first["files"][1]["truncated"], true);
        let rest = page(&c, Some("big"), PAGE_CONTENT_BYTES, None).unwrap();
        assert_eq!(rest["files"][0]["content"].as_str().unwrap().len(), 500);
        assert_eq!(rest["files"][0]["truncated"], false);
        assert_eq!(rest["next_offset"], Value::Null);
    }

    #[test]
    fn a_path_page_continues_on_utf8_boundaries() {
        let body = "\u{00e9}".repeat(10);
        let c = capture(vec![text("u.txt", &body)]);
        let first = page(&c, Some("u.txt"), 0, Some(5)).unwrap();
        assert_eq!(first["files"][0]["content"], "\u{00e9}\u{00e9}");
        assert_eq!(first["next_offset"], 4);
        let mid = page(&c, Some("u.txt"), 5, Some(4)).unwrap();
        assert_eq!(mid["files"][0]["content_offset"], 6);
        assert_eq!(mid["files"][0]["content"], "\u{00e9}\u{00e9}");
    }

    #[test]
    fn an_unknown_path_is_an_error() {
        let c = capture(vec![text("a", "x")]);
        assert!(page(&c, Some("zzz"), 0, None).unwrap_err().contains("zzz"));
    }

    fn run_git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success());
    }

    fn repo_with_changes() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        run_git(p, &["init", "-q", "-b", "main"]);
        run_git(p, &["config", "user.email", "t@t"]);
        run_git(p, &["config", "user.name", "t"]);
        run_git(p, &["config", "commit.gpgsign", "false"]);
        std::fs::write(p.join("keep.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(
            p.join("mv_me.txt"),
            "a long enough body to detect a rename\nline two\nline three\n",
        )
        .unwrap();
        run_git(p, &["add", "-A"]);
        run_git(p, &["commit", "-q", "-m", "base"]);
        std::fs::write(p.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::remove_file(p.join("gone.txt")).unwrap();
        std::fs::rename(p.join("mv_me.txt"), p.join("moved.txt")).unwrap();
        std::fs::write(p.join("img.bin"), [0u8, 159, 146, 150]).unwrap();
        std::fs::write(p.join("caf\u{00e9}.txt"), "cr\u{00e8}me\n").unwrap();
        run_git(p, &["add", "-A"]);
        dir
    }

    #[test]
    fn capture_staged_reads_final_contents_for_every_change_kind() {
        let dir = repo_with_changes();
        let c = capture_staged(dir.path()).unwrap();
        assert_eq!(c.base_ref.as_deref().map(str::len), Some(40));
        let by = |p: &str| c.files.iter().find(|f| f.path == p).unwrap();
        assert_eq!(by("keep.txt").status, ChangeStatus::Modified);
        assert_eq!(
            by("keep.txt").content.as_deref(),
            Some(&b"one\ntwo\nthree\n"[..])
        );
        assert_eq!(by("gone.txt").status, ChangeStatus::Deleted);
        assert!(by("gone.txt").content.is_none());
        assert_eq!(by("moved.txt").status, ChangeStatus::Renamed);
        assert_eq!(by("moved.txt").old_path.as_deref(), Some("mv_me.txt"));
        assert!(by("img.bin").binary);
        assert_eq!(
            by("img.bin").content.as_deref(),
            Some(&[0u8, 159, 146, 150][..])
        );
        assert_eq!(by("caf\u{00e9}.txt").status, ChangeStatus::Added);
        assert!(!by("caf\u{00e9}.txt").binary);
    }

    #[test]
    fn capture_staged_is_none_without_staged_changes() {
        let dir = repo_with_changes();
        run_git(dir.path(), &["reset", "-q", "--hard"]);
        assert!(capture_staged(dir.path()).is_none());
    }
}
