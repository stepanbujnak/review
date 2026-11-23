use crate::git::{DiffFile, FileStatus, StagedFile};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

/// Compares two relative file paths in the exact top-to-bottom order shown in the side panel tree:
/// directories before files at each nesting level, and alphabetically within each group.
pub fn cmp_tree_order(a: &str, b: &str) -> Ordering {
    let a_parts: Vec<&str> = a.split('/').collect();
    let b_parts: Vec<&str> = b.split('/').collect();
    let min_len = a_parts.len().min(b_parts.len());

    for i in 0..min_len {
        let a_is_dir = i + 1 < a_parts.len();
        let b_is_dir = i + 1 < b_parts.len();
        if a_is_dir != b_is_dir {
            return b_is_dir.cmp(&a_is_dir);
        }
        let ord = a_parts[i].cmp(b_parts[i]);
        if ord != Ordering::Equal {
            return ord;
        }
    }

    a_parts.len().cmp(&b_parts.len())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRowKind {
    Dir {
        path: String,
        collapsed: bool,
    },
    File {
        path: String,
        old_path: Option<String>,
        status: FileStatus,
        worktree_status: Option<FileStatus>,
        additions: usize,
        deletions: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub prefix: String,
    pub label: String,
    pub kind: TreeRowKind,
}

#[derive(Default)]
struct DirBuilder {
    dirs: BTreeMap<String, DirBuilder>,
    files: Vec<FileEntry>,
}

struct FileEntry {
    name: String,
    path: String,
    old_path: Option<String>,
    status: FileStatus,
    worktree_status: Option<FileStatus>,
    additions: usize,
    deletions: usize,
}

/// Builds a flattened directory tree with box-drawing connectors.
/// Directories appear before files at each level, both sorted alphabetically.
pub fn build_tree_rows(files: &[&DiffFile], collapsed_dirs: &HashSet<String>) -> Vec<TreeRow> {
    let mut root = DirBuilder::default();

    for file in files {
        insert_file_entry(
            &mut root,
            &file.path,
            file.old_path.clone(),
            file.status,
            None,
            file.additions,
            file.deletions,
        );
    }

    let mut rows = Vec::new();
    flatten_dir(&root, "", "", collapsed_dirs, &mut rows);
    rows
}

/// Builds a flattened directory tree for staged files, preserving worktree update status.
pub fn build_staged_tree_rows(
    files: &[&StagedFile],
    collapsed_dirs: &HashSet<String>,
) -> Vec<TreeRow> {
    let mut root = DirBuilder::default();

    for file in files {
        insert_file_entry(
            &mut root,
            &file.path,
            file.old_path.clone(),
            file.status,
            file.worktree_status,
            0,
            0,
        );
    }

    let mut rows = Vec::new();
    flatten_dir(&root, "", "", collapsed_dirs, &mut rows);
    rows
}

fn insert_file_entry(
    root: &mut DirBuilder,
    path: &str,
    old_path: Option<String>,
    status: FileStatus,
    worktree_status: Option<FileStatus>,
    additions: usize,
    deletions: usize,
) {
    let parts: Vec<&str> = path.split('/').collect();
    let mut cursor = root;
    for dir in &parts[..parts.len().saturating_sub(1)] {
        cursor = cursor.dirs.entry((*dir).to_owned()).or_default();
    }
    let name = parts.last().copied().unwrap_or(path).to_owned();
    cursor.files.push(FileEntry {
        name,
        path: path.to_owned(),
        old_path,
        status,
        worktree_status,
        additions,
        deletions,
    });
}

fn flatten_dir(
    dir: &DirBuilder,
    dir_path: &str,
    indent: &str,
    collapsed_dirs: &HashSet<String>,
    out: &mut Vec<TreeRow>,
) {
    let mut sorted_files: Vec<&FileEntry> = dir.files.iter().collect();
    sorted_files.sort_by(|a, b| a.name.cmp(&b.name));

    let total = dir.dirs.len() + sorted_files.len();
    let mut index = 0;

    for (name, child) in &dir.dirs {
        index += 1;
        let is_last = index == total;
        let child_path = if dir_path.is_empty() {
            name.clone()
        } else {
            format!("{dir_path}/{name}")
        };
        let collapsed = collapsed_dirs.contains(&child_path);
        let connector = if indent.is_empty() {
            ""
        } else if is_last {
            "└─"
        } else {
            "├─"
        };
        let glyph = if collapsed { "▸" } else { "▾" };
        out.push(TreeRow {
            prefix: format!("{indent}{connector}{glyph} "),
            label: format!("{name}/"),
            kind: TreeRowKind::Dir {
                path: child_path.clone(),
                collapsed,
            },
        });

        if !collapsed {
            let next_indent = if indent.is_empty() {
                "  ".to_owned()
            } else if is_last {
                format!("{indent}  ")
            } else {
                format!("{indent}│ ")
            };
            flatten_dir(child, &child_path, &next_indent, collapsed_dirs, out);
        }
    }

    for entry in sorted_files {
        index += 1;
        let is_last = index == total;
        let connector = if indent.is_empty() {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        out.push(TreeRow {
            prefix: format!("{indent}{connector}"),
            label: entry.name.clone(),
            kind: TreeRowKind::File {
                path: entry.path.clone(),
                old_path: entry.old_path.clone(),
                status: entry.status,
                worktree_status: entry.worktree_status,
                additions: entry.additions,
                deletions: entry.deletions,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_file(path: &str, status: FileStatus) -> DiffFile {
        DiffFile {
            path: path.to_owned(),
            old_path: None,
            status,
            additions: 1,
            deletions: 0,
            is_binary: false,
            lines: Vec::new(),
        }
    }

    #[test]
    fn builds_nested_tree_dirs_before_files() {
        let files = [
            stub_file("README.md", FileStatus::Modified),
            stub_file("src/main.rs", FileStatus::Modified),
            stub_file("src/ui/panel.rs", FileStatus::Added),
            stub_file("Cargo.toml", FileStatus::Modified),
        ];
        let refs: Vec<&DiffFile> = files.iter().collect();
        let rows = build_tree_rows(&refs, &HashSet::new());

        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["src/", "ui/", "panel.rs", "main.rs", "Cargo.toml", "README.md"]
        );
    }

    #[test]
    fn respects_collapsed_directories() {
        let files = [
            stub_file("src/main.rs", FileStatus::Modified),
            stub_file("src/git.rs", FileStatus::Modified),
            stub_file("README.md", FileStatus::Modified),
        ];
        let refs: Vec<&DiffFile> = files.iter().collect();
        let mut collapsed = HashSet::new();
        collapsed.insert("src".to_owned());

        let rows = build_tree_rows(&refs, &collapsed);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["src/", "README.md"]);
        assert_eq!(
            rows[0].kind,
            TreeRowKind::Dir {
                path: "src".to_owned(),
                collapsed: true
            }
        );
    }

    #[test]
    fn cmp_tree_order_matches_flattened_tree_file_order() {
        let mut files = [
            stub_file("README.md", FileStatus::Modified),
            stub_file("a_root.rs", FileStatus::Modified),
            stub_file("src/main.rs", FileStatus::Modified),
            stub_file("src/ui/panel.rs", FileStatus::Added),
            stub_file("Cargo.toml", FileStatus::Modified),
        ];
        files.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        let sorted_paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

        let refs: Vec<&DiffFile> = files.iter().collect();
        let tree_paths: Vec<String> = build_tree_rows(&refs, &HashSet::new())
            .into_iter()
            .filter_map(|row| match row.kind {
                TreeRowKind::File { path, .. } => Some(path),
                TreeRowKind::Dir { .. } => None,
            })
            .collect();
        let tree_refs: Vec<&str> = tree_paths.iter().map(String::as_str).collect();

        assert_eq!(
            sorted_paths,
            vec![
                "src/ui/panel.rs",
                "src/main.rs",
                "Cargo.toml",
                "README.md",
                "a_root.rs",
            ]
        );
        assert_eq!(sorted_paths, tree_refs);
    }
}
