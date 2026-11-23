use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Maximum untracked file size to read into memory (2 MiB).
const MAX_UNTRACKED_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

impl FileStatus {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    HunkHeader,
    Context,
    Added,
    Removed,
    Notice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    pub is_binary: bool,
    pub lines: Vec<DiffLine>,
}

impl DiffFile {
    pub fn display_header_path(&self) -> String {
        match &self.old_path {
            Some(old) if old != &self.path => format!("{old} -> {}", self.path),
            _ => self.path.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub worktree_status: Option<FileStatus>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadedDiff {
    pub files: Vec<DiffFile>,
    pub staged: Vec<StagedFile>,
    pub staged_diffs: Vec<DiffFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffTarget {
    /// Working tree changes compared against the index (plus staged status & untracked files).
    WorkingTree,
    /// Staged changes only (`git diff --cached`).
    Staged,
    /// Explicit git diff revision/range arguments (e.g. `main..HEAD`).
    Range(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    pub hide_untracked: bool,
    pub hide_whitespace: bool,
    pub target: DiffTarget,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            hide_untracked: false,
            hide_whitespace: true,
            target: DiffTarget::WorkingTree,
        }
    }
}

/// Finds the git repository top-level directory starting from `cwd`.
pub fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "not inside a git repository".to_owned()
        } else {
            stderr
        });
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(PathBuf::from(root))
}

fn has_head_commit(repo_root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo_root)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Loads changed files (and staged/untracked files in `WorkingTree` mode) for `repo_root`.
pub fn load_diff(repo_root: &Path, opts: &DiffOptions) -> Result<LoadedDiff, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args(["diff", "--no-color", "--no-ext-diff", "-M"]);
    if opts.hide_whitespace {
        cmd.arg("-w");
    }

    match &opts.target {
        DiffTarget::WorkingTree => {
            // Diff index against working tree so staged changes live in the Staged section
            // while unstaged/updated changes appear in the main diff view.
        }
        DiffTarget::Staged => {
            cmd.arg("--cached");
        }
        DiffTarget::Range(args) => {
            cmd.args(args);
        }
    }

    let output = cmd
        .output()
        .map_err(|err| format!("failed to run git diff: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "git diff failed".to_owned()
        } else {
            stderr
        });
    }

    let raw_diff = String::from_utf8_lossy(&output.stdout);
    let mut files = parse_unified_diff(&raw_diff);

    let (staged, staged_diffs) = if matches!(opts.target, DiffTarget::WorkingTree) {
        if !opts.hide_untracked {
            let untracked = load_untracked_files(repo_root)?;
            for file in untracked {
                if !files.iter().any(|existing| existing.path == file.path) {
                    files.push(file);
                }
            }
        }
        let staged = load_staged_files(repo_root)?;
        let staged_diffs = if staged.is_empty() {
            Vec::new()
        } else {
            load_staged_diffs(repo_root, opts.hide_whitespace)?
        };
        (staged, staged_diffs)
    } else {
        (Vec::new(), Vec::new())
    };

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(LoadedDiff {
        files,
        staged,
        staged_diffs,
    })
}

fn load_staged_diffs(repo_root: &Path, hide_whitespace: bool) -> Result<Vec<DiffFile>, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args(["diff", "--cached", "--no-color", "--no-ext-diff", "-M"]);
    if hide_whitespace {
        cmd.arg("-w");
    }

    let output = cmd
        .output()
        .map_err(|err| format!("failed to run git diff --cached: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "git diff --cached failed".to_owned()
        } else {
            stderr
        });
    }

    let raw_diff = String::from_utf8_lossy(&output.stdout);
    let mut files = parse_unified_diff(&raw_diff);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Stages a working tree file (or renamed pair) into the Git index.
pub fn stage_file(repo_root: &Path, path: &str, old_path: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args(["add", "--"]);
    if let Some(old) = old_path
        && old != path
    {
        cmd.arg(old);
    }
    cmd.arg(path);

    let output = cmd
        .output()
        .map_err(|err| format!("failed to run git add: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("git add failed for {path}")
        } else {
            stderr
        });
    }

    Ok(())
}

/// Unstages a file (or renamed pair) from the Git index while preserving working tree edits.
pub fn unstage_file(repo_root: &Path, path: &str, old_path: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    if has_head_commit(repo_root) {
        cmd.args(["reset", "-q", "HEAD", "--"]);
    } else {
        cmd.args(["rm", "--cached", "-q", "--"]);
    }
    if let Some(old) = old_path
        && old != path
    {
        cmd.arg(old);
    }
    cmd.arg(path);

    let output = cmd
        .output()
        .map_err(|err| format!("failed to unstage file: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("failed to unstage {path}")
        } else {
            stderr
        });
    }

    Ok(())
}

fn load_staged_files(repo_root: &Path) -> Result<Vec<StagedFile>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "-uno"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| format!("failed to run git status: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "git status failed".to_owned()
        } else {
            stderr
        });
    }

    Ok(parse_porcelain_staged(&output.stdout))
}

fn porcelain_index_status(code: u8) -> Option<FileStatus> {
    match code {
        b'M' | b'T' => Some(FileStatus::Modified),
        b'A' | b'C' => Some(FileStatus::Added),
        b'D' => Some(FileStatus::Deleted),
        b'R' => Some(FileStatus::Renamed),
        _ => None,
    }
}

fn porcelain_worktree_status(code: u8) -> Option<FileStatus> {
    match code {
        b'M' | b'T' => Some(FileStatus::Modified),
        b'D' => Some(FileStatus::Deleted),
        b'A' => Some(FileStatus::Added),
        b'R' => Some(FileStatus::Renamed),
        _ => None,
    }
}

/// Parses `git status --porcelain=v1 -z` output and returns all files with staged changes,
/// including any subsequent working-tree updates (`worktree_status`).
pub fn parse_porcelain_staged(bytes: &[u8]) -> Vec<StagedFile> {
    let mut staged = Vec::new();
    let mut entries = bytes.split(|&b| b == 0).peekable();

    while let Some(entry) = entries.next() {
        if entry.len() < 4 || entry[2] != b' ' {
            continue;
        }
        let x = entry[0];
        let y = entry[1];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let has_orig_path = matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C');
        let old_path = if has_orig_path {
            entries
                .next()
                .map(|raw| String::from_utf8_lossy(raw).into_owned())
        } else {
            None
        };

        if let Some(status) = porcelain_index_status(x) {
            let worktree_status = porcelain_worktree_status(y);
            staged.push(StagedFile {
                path,
                old_path,
                status,
                worktree_status,
            });
        }
    }

    staged.sort_by(|a, b| a.path.cmp(&b.path));
    staged
}

fn load_untracked_files(repo_root: &Path) -> Result<Vec<DiffFile>, String> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| format!("failed to list untracked files: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "git ls-files failed".to_owned()
        } else {
            stderr
        });
    }

    let mut result = Vec::new();
    for raw_path in output.stdout.split(|&b| b == 0) {
        if raw_path.is_empty() {
            continue;
        }
        let rel_path = String::from_utf8_lossy(raw_path).into_owned();
        let full_path = repo_root.join(&rel_path);
        result.push(build_untracked_diff_file(rel_path, &full_path));
    }

    Ok(result)
}

fn build_untracked_diff_file(rel_path: String, full_path: &Path) -> DiffFile {
    let meta = match fs::metadata(full_path) {
        Ok(m) => m,
        Err(err) => {
            return DiffFile {
                path: rel_path,
                old_path: None,
                status: FileStatus::Untracked,
                additions: 0,
                deletions: 0,
                is_binary: false,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Notice,
                    text: format!("(unreadable untracked file: {err})"),
                }],
            };
        }
    };

    if meta.len() > MAX_UNTRACKED_BYTES {
        return DiffFile {
            path: rel_path,
            old_path: None,
            status: FileStatus::Untracked,
            additions: 0,
            deletions: 0,
            is_binary: false,
            lines: vec![DiffLine {
                kind: DiffLineKind::Notice,
                text: format!("(untracked file too large to preview: {} bytes)", meta.len()),
            }],
        };
    }

    let bytes = match fs::read(full_path) {
        Ok(b) => b,
        Err(err) => {
            return DiffFile {
                path: rel_path,
                old_path: None,
                status: FileStatus::Untracked,
                additions: 0,
                deletions: 0,
                is_binary: false,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Notice,
                    text: format!("(failed to read untracked file: {err})"),
                }],
            };
        }
    };

    let sample_len = bytes.len().min(8192);
    if bytes[..sample_len].contains(&0) {
        return DiffFile {
            path: rel_path,
            old_path: None,
            status: FileStatus::Untracked,
            additions: 0,
            deletions: 0,
            is_binary: true,
            lines: vec![DiffLine {
                kind: DiffLineKind::Notice,
                text: "Binary file (untracked)".to_owned(),
            }],
        };
    }

    if bytes.is_empty() {
        return DiffFile {
            path: rel_path,
            old_path: None,
            status: FileStatus::Untracked,
            additions: 0,
            deletions: 0,
            is_binary: false,
            lines: vec![DiffLine {
                kind: DiffLineKind::Notice,
                text: "(empty untracked file)".to_owned(),
            }],
        };
    }

    let text = String::from_utf8_lossy(&bytes);
    let raw_lines: Vec<&str> = text.lines().collect();
    let count = raw_lines.len();
    let mut lines = Vec::with_capacity(count + 1);
    lines.push(DiffLine {
        kind: DiffLineKind::HunkHeader,
        text: format!("@@ -0,0 +1,{count} @@"),
    });
    for line in raw_lines {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("+{line}"),
        });
    }

    DiffFile {
        path: rel_path,
        old_path: None,
        status: FileStatus::Untracked,
        additions: count,
        deletions: 0,
        is_binary: false,
        lines,
    }
}

/// Parses `git diff --no-color --no-ext-diff -M` output into structured per-file diffs.
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current: Option<DiffFile> = None;
    let mut in_hunk = false;

    for raw_line in input.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(finalize_file(file));
            }
            let (old_p, new_p) = parse_diff_git_header(rest);
            let old_path = if old_p != new_p { Some(old_p) } else { None };
            current = Some(DiffFile {
                path: new_p,
                old_path,
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
                is_binary: false,
                lines: Vec::new(),
            });
            in_hunk = false;
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        if !in_hunk {
            if line.starts_with("@@ ") {
                in_hunk = true;
                file.lines.push(DiffLine {
                    kind: DiffLineKind::HunkHeader,
                    text: line.to_owned(),
                });
            } else if line.starts_with("new file mode ") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode ") {
                file.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = Some(unquote_git_path(from));
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.status = FileStatus::Renamed;
                file.path = unquote_git_path(to);
            } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
                file.lines.push(DiffLine {
                    kind: DiffLineKind::Notice,
                    text: line.to_owned(),
                });
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                file.is_binary = true;
                file.lines.push(DiffLine {
                    kind: DiffLineKind::Notice,
                    text: line.to_owned(),
                });
            } else if let Some(p) = line.strip_prefix("+++ ") {
                let cleaned = strip_ab_prefix(&unquote_git_path(p));
                if cleaned != "/dev/null" && !cleaned.is_empty() {
                    file.path = cleaned;
                }
            } else if let Some(p) = line.strip_prefix("--- ") {
                let cleaned = strip_ab_prefix(&unquote_git_path(p));
                if cleaned != "/dev/null" && !cleaned.is_empty() && file.status == FileStatus::Deleted {
                    file.path = cleaned;
                }
            }
            continue;
        }

        if line.starts_with("@@ ") {
            file.lines.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                text: line.to_owned(),
            });
        } else if line.starts_with('+') {
            file.additions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: line.to_owned(),
            });
        } else if line.starts_with('-') {
            file.deletions += 1;
            file.lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: line.to_owned(),
            });
        } else if line.starts_with('\\') {
            file.lines.push(DiffLine {
                kind: DiffLineKind::Notice,
                text: line.to_owned(),
            });
        } else {
            file.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                text: line.to_owned(),
            });
        }
    }

    if let Some(file) = current.take() {
        files.push(finalize_file(file));
    }

    files
}

fn finalize_file(mut file: DiffFile) -> DiffFile {
    if file.lines.is_empty() {
        let notice = match file.status {
            FileStatus::Added => "(empty new file)",
            FileStatus::Deleted => "(empty deleted file)",
            FileStatus::Renamed => "(renamed without content changes)",
            _ => "(no textual diff)",
        };
        file.lines.push(DiffLine {
            kind: DiffLineKind::Notice,
            text: notice.to_owned(),
        });
    }
    file
}

fn parse_diff_git_header(rest: &str) -> (String, String) {
    // Typical format: `a/path/to/file b/path/to/file` or quoted `"a/..." "b/..."`.
    let tokens = split_git_header_paths(rest);
    if tokens.len() >= 2 {
        let old = strip_ab_prefix(&unquote_git_path(&tokens[0]));
        let new = strip_ab_prefix(&unquote_git_path(&tokens[1]));
        return (old, new);
    }
    let fallback = strip_ab_prefix(&unquote_git_path(rest.trim()));
    (fallback.clone(), fallback)
}

fn split_git_header_paths(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' {
            chars.next();
            continue;
        }
        if c == '"' {
            let mut token = String::new();
            token.push(chars.next().unwrap());
            let mut escaped = false;
            for ch in chars.by_ref() {
                token.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                }
            }
            out.push(token);
        } else {
            // Unquoted paths may contain spaces; split on " b/" boundary when possible.
            let remainder: String = chars.collect();
            if let Some(idx) = remainder.find(" b/") {
                out.push(remainder[..idx].to_owned());
                out.push(remainder[idx + 1..].to_owned());
            } else if let Some((a, b)) = remainder.split_once(' ') {
                out.push(a.to_owned());
                out.push(b.to_owned());
            } else {
                out.push(remainder);
            }
            break;
        }
    }
    out
}

fn strip_ab_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        stripped.to_owned()
    } else {
        path.to_owned()
    }
}

fn unquote_git_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => break,
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("review-test-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    #[test]
    fn parses_multi_file_unified_diff() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"extra\");
 }
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
diff --git a/image.png b/image.png
new file mode 100644
Binary files /dev/null and b/image.png differ
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 3);

        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[0].lines[0].kind, DiffLineKind::HunkHeader);

        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].old_path.as_deref(), Some("old.txt"));
        assert_eq!(files[1].status, FileStatus::Renamed);
        assert_eq!(files[1].display_header_path(), "old.txt -> new.txt");

        assert_eq!(files[2].path, "image.png");
        assert_eq!(files[2].status, FileStatus::Added);
        assert!(files[2].is_binary);
    }

    #[test]
    fn integration_whitespace_and_untracked_toggles() {
        let dir = make_temp_dir("toggles");
        run_git(&dir, &["init"]);
        fs::write(dir.join("hello.txt"), "hello world\n").unwrap();
        run_git(&dir, &["add", "hello.txt"]);
        run_git(&dir, &["commit", "-m", "init"]);

        // Whitespace-only edit + untracked text file + untracked binary file.
        fs::write(dir.join("hello.txt"), "hello   world\n").unwrap();
        fs::write(dir.join("notes.txt"), "line 1\nline 2\n").unwrap();
        fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();

        // Default: hide_whitespace = true, hide_untracked = false.
        let default_loaded = load_diff(&dir, &DiffOptions::default()).unwrap();
        let names: Vec<&str> = default_loaded.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["blob.bin", "notes.txt"]);
        assert!(default_loaded.files[0].is_binary);
        assert_eq!(default_loaded.files[1].status, FileStatus::Untracked);
        assert_eq!(default_loaded.files[1].additions, 2);
        assert!(default_loaded.staged.is_empty());

        // Show whitespace changes (hide_whitespace = false).
        let with_ws = load_diff(
            &dir,
            &DiffOptions {
                hide_whitespace: false,
                hide_untracked: false,
                target: DiffTarget::WorkingTree,
            },
        )
        .unwrap();
        assert!(with_ws.files.iter().any(|f| f.path == "hello.txt"));

        // Hide untracked files (hide_untracked = true, hide_whitespace = true).
        let no_untracked = load_diff(
            &dir,
            &DiffOptions {
                hide_whitespace: true,
                hide_untracked: true,
                target: DiffTarget::WorkingTree,
            },
        )
        .unwrap();
        assert!(no_untracked.files.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn integration_unborn_head_works() {
        let dir = make_temp_dir("unborn");
        run_git(&dir, &["init"]);
        fs::write(dir.join("staged.txt"), "a\nb\n").unwrap();
        run_git(&dir, &["add", "staged.txt"]);
        fs::write(dir.join("untracked.txt"), "c\n").unwrap();

        let loaded = load_diff(&dir, &DiffOptions::default()).unwrap();
        let changed_names: Vec<&str> = loaded.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(changed_names, vec!["untracked.txt"]);
        assert_eq!(loaded.staged.len(), 1);
        assert_eq!(loaded.staged[0].path, "staged.txt");
        assert_eq!(loaded.staged[0].status, FileStatus::Added);
        assert_eq!(loaded.staged[0].worktree_status, None);

        // Unstaging on unborn HEAD removes from index while keeping the file untracked.
        unstage_file(&dir, "staged.txt", None).unwrap();
        let after_unstage = load_diff(&dir, &DiffOptions::default()).unwrap();
        assert!(after_unstage.staged.is_empty());
        let names: Vec<&str> = after_unstage.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["staged.txt", "untracked.txt"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn integration_stage_update_and_unstage_workflow() {
        let dir = make_temp_dir("stage-workflow");
        run_git(&dir, &["init"]);
        fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        fs::write(dir.join("to_delete.txt"), "bye\n").unwrap();
        run_git(&dir, &["add", "tracked.txt", "to_delete.txt"]);
        run_git(&dir, &["commit", "-m", "init"]);

        // Modify tracked, delete to_delete, create untracked.
        fs::write(dir.join("tracked.txt"), "v2\n").unwrap();
        fs::remove_file(dir.join("to_delete.txt")).unwrap();
        fs::write(dir.join("new.txt"), "hello\n").unwrap();

        // Stage all three one by one.
        stage_file(&dir, "tracked.txt", None).unwrap();
        stage_file(&dir, "to_delete.txt", None).unwrap();
        stage_file(&dir, "new.txt", None).unwrap();

        let loaded = load_diff(&dir, &DiffOptions::default()).unwrap();
        assert!(loaded.files.is_empty());
        assert_eq!(loaded.staged.len(), 3);
        assert_eq!(
            loaded.staged[0],
            StagedFile {
                path: "new.txt".to_owned(),
                old_path: None,
                status: FileStatus::Added,
                worktree_status: None,
            }
        );
        assert_eq!(
            loaded.staged[1],
            StagedFile {
                path: "to_delete.txt".to_owned(),
                old_path: None,
                status: FileStatus::Deleted,
                worktree_status: None,
            }
        );
        assert_eq!(
            loaded.staged[2],
            StagedFile {
                path: "tracked.txt".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                worktree_status: None,
            }
        );

        // Update tracked.txt and new.txt after staging -> should show worktree_status = Modified
        // and reappear in unstaged diff.
        fs::write(dir.join("tracked.txt"), "v3\n").unwrap();
        fs::write(dir.join("new.txt"), "hello again\n").unwrap();

        let updated = load_diff(&dir, &DiffOptions::default()).unwrap();
        let changed_names: Vec<&str> = updated.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(changed_names, vec!["new.txt", "tracked.txt"]);
        assert_eq!(updated.staged[0].status, FileStatus::Added);
        assert_eq!(updated.staged[0].worktree_status, Some(FileStatus::Modified));
        assert_eq!(updated.staged[2].status, FileStatus::Modified);
        assert_eq!(updated.staged[2].worktree_status, Some(FileStatus::Modified));

        // Unstage tracked.txt -> removed from staged, still present in changed with full v1->v3 diff.
        unstage_file(&dir, "tracked.txt", None).unwrap();
        let after_unstage = load_diff(&dir, &DiffOptions::default()).unwrap();
        assert!(!after_unstage.staged.iter().any(|f| f.path == "tracked.txt"));
        assert!(after_unstage.files.iter().any(|f| f.path == "tracked.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_porcelain_staged_including_renames_and_updates() {
        let raw = b"M  clean.rs\0MM updated.rs\0AM added_mod.rs\0AD added_del.rs\0R  new_name.rs\0old_name.rs\0 M unstaged.rs\0?? untracked.rs\0";
        let staged = parse_porcelain_staged(raw);
        assert_eq!(staged.len(), 5);
        assert_eq!(staged[0].path, "added_del.rs");
        assert_eq!(staged[0].status, FileStatus::Added);
        assert_eq!(staged[0].worktree_status, Some(FileStatus::Deleted));

        assert_eq!(staged[1].path, "added_mod.rs");
        assert_eq!(staged[1].status, FileStatus::Added);
        assert_eq!(staged[1].worktree_status, Some(FileStatus::Modified));

        assert_eq!(staged[2].path, "clean.rs");
        assert_eq!(staged[2].status, FileStatus::Modified);
        assert_eq!(staged[2].worktree_status, None);

        assert_eq!(staged[3].path, "new_name.rs");
        assert_eq!(staged[3].old_path.as_deref(), Some("old_name.rs"));
        assert_eq!(staged[3].status, FileStatus::Renamed);
        assert_eq!(staged[3].worktree_status, None);

        assert_eq!(staged[4].path, "updated.rs");
        assert_eq!(staged[4].status, FileStatus::Modified);
        assert_eq!(staged[4].worktree_status, Some(FileStatus::Modified));
    }
}
