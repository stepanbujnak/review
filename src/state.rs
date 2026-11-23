use crate::git::{DiffFile, DiffLine, DiffLineKind, DiffOptions, DiffTarget, LoadedDiff, StagedFile};
use crate::tree::cmp_tree_order;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ActiveItem {
    Staged(String),
    Changed(String),
}

impl ActiveItem {
    pub fn path(&self) -> &str {
        match self {
            Self::Staged(p) | Self::Changed(p) => p,
        }
    }

    pub fn is_staged(&self) -> bool {
        matches!(self, Self::Staged(_))
    }
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub files: Vec<DiffFile>,
    pub staged: Vec<StagedFile>,
    pub staged_diffs: Vec<DiffFile>,
    pub hide_untracked: bool,
    pub hide_whitespace: bool,
    pub hide_side_panel: bool,
    pub wrap_lines: bool,
    pub active_item: Option<ActiveItem>,
    pub collapsed_dirs: HashSet<String>,
    pub collapsed_staged_dirs: HashSet<String>,
    pub target: DiffTarget,
}

impl AppState {
    pub fn new(mut loaded: LoadedDiff, target: DiffTarget) -> Self {
        loaded.files.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        loaded.staged.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        loaded
            .staged_diffs
            .sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        let active_item = loaded
            .files
            .first()
            .map(|f| ActiveItem::Changed(f.path.clone()));
        Self {
            files: loaded.files,
            staged: loaded.staged,
            staged_diffs: loaded.staged_diffs,
            hide_untracked: false,
            hide_whitespace: true,
            hide_side_panel: false,
            wrap_lines: false,
            active_item,
            collapsed_dirs: HashSet::new(),
            collapsed_staged_dirs: HashSet::new(),
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

    pub fn can_stage(&self) -> bool {
        matches!(self.target, DiffTarget::WorkingTree)
    }

    pub fn changed_files(&self) -> Vec<&DiffFile> {
        self.files.iter().collect()
    }

    pub fn staged_files(&self) -> Vec<&StagedFile> {
        self.staged.iter().collect()
    }

    /// Returns a `DiffFile` for every staged file (in tree order), falling back to a notice
    /// placeholder if the staged diff was omitted (e.g. whitespace-only under `-w`).
    pub fn staged_diff_files(&self) -> Vec<DiffFile> {
        self.staged
            .iter()
            .map(|sf| {
                self.staged_diffs
                    .iter()
                    .find(|df| df.path == sf.path)
                    .cloned()
                    .unwrap_or_else(|| DiffFile {
                        path: sf.path.clone(),
                        old_path: sf.old_path.clone(),
                        status: sf.status,
                        additions: 0,
                        deletions: 0,
                        is_binary: false,
                        lines: vec![DiffLine {
                            kind: DiffLineKind::Notice,
                            text: "(staged change — no textual diff shown)".to_owned(),
                        }],
                    })
            })
            .collect()
    }

    pub fn changed_count(&self) -> usize {
        self.files.len()
    }

    pub fn staged_count(&self) -> usize {
        self.staged.len()
    }

    pub fn viewing_staged(&self) -> bool {
        self.active_item.as_ref().is_some_and(ActiveItem::is_staged)
    }

    pub fn active_path(&self) -> Option<&str> {
        self.active_item.as_ref().map(ActiveItem::path)
    }

    pub fn active_changed_file(&self) -> Option<&DiffFile> {
        let ActiveItem::Changed(path) = self.active_item.as_ref()? else {
            return None;
        };
        self.files.iter().find(|f| &f.path == path)
    }

    pub fn active_staged_file(&self) -> Option<&StagedFile> {
        let ActiveItem::Staged(path) = self.active_item.as_ref()? else {
            return None;
        };
        self.staged.iter().find(|f| &f.path == path)
    }

    /// Returns the item that should become active after staging `path`
    /// (the next changed file, or the previous one if `path` was last, or `None`).
    pub fn next_item_after_staging(&self, path: &str) -> Option<ActiveItem> {
        let pos = self.files.iter().position(|f| f.path == path)?;
        if pos + 1 < self.files.len() {
            Some(ActiveItem::Changed(self.files[pos + 1].path.clone()))
        } else if pos > 0 {
            Some(ActiveItem::Changed(self.files[pos - 1].path.clone()))
        } else {
            None
        }
    }

    /// Returns the item that should become active after unstaging `path`
    /// (the next staged file, or the previous staged file, or `Changed(path)` when no staged files remain).
    pub fn next_item_after_unstaging(&self, path: &str) -> Option<ActiveItem> {
        let pos = self.staged.iter().position(|f| f.path == path)?;
        if pos + 1 < self.staged.len() {
            Some(ActiveItem::Staged(self.staged[pos + 1].path.clone()))
        } else if pos > 0 {
            Some(ActiveItem::Staged(self.staged[pos - 1].path.clone()))
        } else {
            Some(ActiveItem::Changed(path.to_owned()))
        }
    }

    /// Returns the next (+1) or previous (-1) file across Staged (top) and Changed (bottom).
    pub fn next_active_item(&self, delta: isize) -> Option<ActiveItem> {
        let mut items: Vec<ActiveItem> = Vec::with_capacity(self.staged.len() + self.files.len());
        if self.can_stage() {
            items.extend(self.staged.iter().map(|f| ActiveItem::Staged(f.path.clone())));
        }
        items.extend(self.files.iter().map(|f| ActiveItem::Changed(f.path.clone())));

        if items.is_empty() {
            return None;
        }

        let Some(active) = self.active_item.as_ref() else {
            return if delta < 0 {
                items.last().cloned()
            } else {
                items.first().cloned()
            };
        };

        let current_idx = items.iter().position(|item| item == active).unwrap_or(0);
        let len = items.len() as isize;
        let next_idx = (current_idx as isize + delta).clamp(0, len - 1) as usize;
        Some(items[next_idx].clone())
    }

    pub fn toggle_dir(&mut self, dir_path: &str) {
        if !self.collapsed_dirs.remove(dir_path) {
            self.collapsed_dirs.insert(dir_path.to_owned());
        }
    }

    pub fn toggle_staged_dir(&mut self, dir_path: &str) {
        if !self.collapsed_staged_dirs.remove(dir_path) {
            self.collapsed_staged_dirs.insert(dir_path.to_owned());
        }
    }

    fn item_exists(&self, item: &ActiveItem) -> bool {
        match item {
            ActiveItem::Staged(p) => self.staged.iter().any(|f| &f.path == p),
            ActiveItem::Changed(p) => self.files.iter().any(|f| &f.path == p),
        }
    }

    /// Replaces changed and staged file lists after a git reload.
    pub fn reload_files(&mut self, mut loaded: LoadedDiff, preferred_active: Option<ActiveItem>) {
        loaded.files.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        loaded.staged.sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        loaded
            .staged_diffs
            .sort_by(|a, b| cmp_tree_order(&a.path, &b.path));
        self.files = loaded.files;
        self.staged = loaded.staged;
        self.staged_diffs = loaded.staged_diffs;

        if let Some(pref) = preferred_active
            && self.item_exists(&pref)
        {
            self.active_item = Some(pref);
            return;
        }

        let still_present = self
            .active_item
            .as_ref()
            .is_some_and(|active| self.item_exists(active));
        if !still_present {
            self.active_item = self
                .files
                .first()
                .map(|f| ActiveItem::Changed(f.path.clone()));
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

    fn stub_staged(path: &str, status: FileStatus, worktree_status: Option<FileStatus>) -> StagedFile {
        StagedFile {
            path: path.to_owned(),
            old_path: None,
            status,
            worktree_status,
        }
    }

    #[test]
    fn defaults_match_goal_spec() {
        let state = AppState::new(
            LoadedDiff {
                files: vec![stub_file("a.rs")],
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        assert!(!state.hide_untracked);
        assert!(state.hide_whitespace);
        assert!(!state.hide_side_panel);
        assert!(!state.wrap_lines);
        assert!(state.can_stage());
        assert_eq!(state.active_item, Some(ActiveItem::Changed("a.rs".to_owned())));
    }

    #[test]
    fn next_and_prev_navigate_across_staged_and_changed() {
        let mut state = AppState::new(
            LoadedDiff {
                files: vec![stub_file("c.rs"), stub_file("d.rs")],
                staged: vec![
                    stub_staged("a.rs", FileStatus::Modified, None),
                    stub_staged("b.rs", FileStatus::Added, None),
                ],
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        assert_eq!(state.active_item, Some(ActiveItem::Changed("c.rs".to_owned())));

        // Pressing 'p' from first changed file moves up into the last staged file.
        let prev = state.next_active_item(-1);
        assert_eq!(prev, Some(ActiveItem::Staged("b.rs".to_owned())));
        state.active_item = prev;

        // Pressing 'p' again moves to first staged file.
        let prev = state.next_active_item(-1);
        assert_eq!(prev, Some(ActiveItem::Staged("a.rs".to_owned())));
        state.active_item = prev;

        // Pressing 'n' moves forward through staged and back into changed.
        state.active_item = state.next_active_item(1);
        assert_eq!(state.active_item, Some(ActiveItem::Staged("b.rs".to_owned())));
        state.active_item = state.next_active_item(1);
        assert_eq!(state.active_item, Some(ActiveItem::Changed("c.rs".to_owned())));
        state.active_item = state.next_active_item(1);
        assert_eq!(state.active_item, Some(ActiveItem::Changed("d.rs".to_owned())));
    }

    #[test]
    fn next_item_after_staging_and_unstaging() {
        let state = AppState::new(
            LoadedDiff {
                files: vec![stub_file("c.rs"), stub_file("d.rs")],
                staged: vec![
                    stub_staged("a.rs", FileStatus::Modified, None),
                    stub_staged("b.rs", FileStatus::Modified, None),
                ],
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );

        assert_eq!(
            state.next_item_after_staging("c.rs"),
            Some(ActiveItem::Changed("d.rs".to_owned()))
        );
        assert_eq!(
            state.next_item_after_staging("d.rs"),
            Some(ActiveItem::Changed("c.rs".to_owned()))
        );
        assert_eq!(
            state.next_item_after_unstaging("a.rs"),
            Some(ActiveItem::Staged("b.rs".to_owned()))
        );
        assert_eq!(
            state.next_item_after_unstaging("b.rs"),
            Some(ActiveItem::Staged("a.rs".to_owned()))
        );
    }

    #[test]
    fn reload_updates_staged_and_respects_preferred_active() {
        let mut state = AppState::new(
            LoadedDiff {
                files: vec![stub_file("a.rs"), stub_file("b.rs")],
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );

        // Simulate staging a.rs with preferred next = Changed(b.rs).
        state.reload_files(
            LoadedDiff {
                files: vec![stub_file("b.rs")],
                staged: vec![stub_staged("a.rs", FileStatus::Modified, None)],
                staged_diffs: Vec::new(),
            },
            Some(ActiveItem::Changed("b.rs".to_owned())),
        );
        assert_eq!(state.staged_count(), 1);
        assert_eq!(state.changed_count(), 1);
        assert_eq!(state.active_item, Some(ActiveItem::Changed("b.rs".to_owned())));

        // Simulate unstaging a.rs with preferred active = Changed(a.rs).
        state.reload_files(
            LoadedDiff {
                files: vec![stub_file("a.rs"), stub_file("b.rs")],
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            Some(ActiveItem::Changed("a.rs".to_owned())),
        );
        assert_eq!(state.staged_count(), 0);
        assert_eq!(state.changed_count(), 2);
        assert_eq!(state.active_item, Some(ActiveItem::Changed("a.rs".to_owned())));
    }
}
