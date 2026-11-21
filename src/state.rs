use crate::git::{DiffFile, DiffOptions, DiffTarget};
use crate::tree::cmp_tree_order;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct AppState {
    pub files: Vec<DiffFile>,
    pub reviewed: HashSet<String>,
    pub hide_untracked: bool,
    pub hide_whitespace: bool,
    pub hide_side_panel: bool,
    pub wrap_lines: bool,
    pub active_file: Option<String>,
    pub collapsed_dirs: HashSet<String>,
    pub target: DiffTarget,
}

impl AppState {
    pub fn new(mut files: Vec<DiffFile>, target: DiffTarget) -> Self {
        files.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        let active_file = files.first().map(|f| f.path.clone());
        Self {
            files,
            reviewed: HashSet::new(),
            hide_untracked: false,
            hide_whitespace: true,
            hide_side_panel: false,
            wrap_lines: false,
            active_file,
            collapsed_dirs: HashSet::new(),
            target,
        }
    }

    pub fn diff_options(&self) -> DiffOptions {
        DiffOptions {
            hide_untracked: self.hide_untracked,
            hide_whitespace: self.hide_whitespace,
            target: self.target.clone(),
        }
    }

    pub fn unreviewed_files(&self) -> Vec<&DiffFile> {
        self.files
            .iter()
            .filter(|f| !self.reviewed.contains(&f.path))
            .collect()
    }

    pub fn reviewed_files(&self) -> Vec<&DiffFile> {
        self.files
            .iter()
            .filter(|f| self.reviewed.contains(&f.path))
            .collect()
    }

    pub fn total_count(&self) -> usize {
        self.files.len()
    }

    pub fn reviewed_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| self.reviewed.contains(&f.path))
            .count()
    }

    /// Marks the currently viewed file as reviewed and advances `active_file`
    /// to the next unreviewed file (or the previous one if it was last).
    pub fn mark_active_reviewed(&mut self) -> Option<String> {
        let current = self.active_file.clone()?;
        if self.reviewed.contains(&current) {
            return None;
        }

        let unreviewed_paths: Vec<String> = self
            .files
            .iter()
            .filter(|f| !self.reviewed.contains(&f.path))
            .map(|f| f.path.clone())
            .collect();

        let pos = unreviewed_paths.iter().position(|p| p == &current)?;
        self.reviewed.insert(current.clone());

        let next_active = if pos + 1 < unreviewed_paths.len() {
            Some(unreviewed_paths[pos + 1].clone())
        } else if pos > 0 {
            Some(unreviewed_paths[pos - 1].clone())
        } else {
            None
        };
        self.active_file = next_active;
        Some(current)
    }

    /// Un-marks a reviewed file and makes it the active file.
    pub fn unmark_reviewed(&mut self, path: &str) -> bool {
        if self.reviewed.remove(path) {
            self.active_file = Some(path.to_owned());
            true
        } else {
            false
        }
    }

    /// Returns the next (+1) or previous (-1) unreviewed file relative to `active_file`.
    pub fn next_active_file(&self, delta: isize) -> Option<String> {
        let paths: Vec<String> = self
            .unreviewed_files()
            .into_iter()
            .map(|f| f.path.clone())
            .collect();
        if paths.is_empty() {
            return None;
        }
        let current_idx = self
            .active_file
            .as_ref()
            .and_then(|active| paths.iter().position(|p| p == active))
            .unwrap_or(0);
        let len = paths.len() as isize;
        let next_idx = (current_idx as isize + delta).clamp(0, len - 1) as usize;
        Some(paths[next_idx].clone())
    }

    pub fn toggle_dir(&mut self, dir_path: &str) {
        if !self.collapsed_dirs.remove(dir_path) {
            self.collapsed_dirs.insert(dir_path.to_owned());
        }
    }

    /// Replaces the file list after a git reload while preserving `reviewed` paths.
    pub fn reload_files(&mut self, mut files: Vec<DiffFile>) {
        files.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        self.files = files;
        let still_unreviewed = self.active_file.as_ref().is_some_and(|active| {
            self.files
                .iter()
                .any(|f| &f.path == active && !self.reviewed.contains(&f.path))
        });
        if !still_unreviewed {
            self.active_file = self
                .unreviewed_files()
                .first()
                .map(|f| f.path.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileStatus;

    fn stub_file(path: &str) -> DiffFile {
        DiffFile {
            path: path.to_owned(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 1,
            deletions: 0,
            is_binary: false,
            lines: Vec::new(),
        }
    }

    #[test]
    fn defaults_match_goal_spec() {
        let state = AppState::new(vec![stub_file("a.rs")], DiffTarget::WorkingTree);
        assert!(!state.hide_untracked);
        assert!(state.hide_whitespace);
        assert!(!state.hide_side_panel);
        assert!(!state.wrap_lines);
        assert_eq!(state.active_file.as_deref(), Some("a.rs"));
    }

    #[test]
    fn mark_and_unmark_reviewed_updates_active_file() {
        let mut state = AppState::new(
            vec![stub_file("a.rs"), stub_file("b.rs"), stub_file("c.rs")],
            DiffTarget::WorkingTree,
        );

        assert_eq!(state.mark_active_reviewed().as_deref(), Some("a.rs"));
        assert_eq!(state.active_file.as_deref(), Some("b.rs"));
        assert_eq!(state.unreviewed_files().len(), 2);
        assert_eq!(state.reviewed_files().len(), 1);

        // Jump to last file and mark reviewed -> falls back to previous remaining file.
        state.active_file = Some("c.rs".to_owned());
        assert_eq!(state.mark_active_reviewed().as_deref(), Some("c.rs"));
        assert_eq!(state.active_file.as_deref(), Some("b.rs"));

        // Unmark a.rs -> becomes active again.
        assert!(state.unmark_reviewed("a.rs"));
        assert_eq!(state.active_file.as_deref(), Some("a.rs"));
        assert_eq!(state.unreviewed_files().len(), 2);
    }

    #[test]
    fn reload_preserves_reviewed_set() {
        let mut state = AppState::new(
            vec![stub_file("a.rs"), stub_file("b.rs")],
            DiffTarget::WorkingTree,
        );
        state.mark_active_reviewed();
        assert!(state.reviewed.contains("a.rs"));

        state.reload_files(vec![
            stub_file("a.rs"),
            stub_file("b.rs"),
            stub_file("c.rs"),
        ]);
        assert!(state.reviewed.contains("a.rs"));
        assert_eq!(state.unreviewed_files().len(), 2);
        assert_eq!(state.active_file.as_deref(), Some("b.rs"));
    }
}
