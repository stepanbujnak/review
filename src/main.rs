mod git;
mod state;
mod tree;
mod ui;

use crate::git::{DiffTarget, load_diff, repo_root, stage_file, unstage_file};
use crate::state::AppState;
use opentui::prelude::*;
use std::env;
use std::path::Path;
use std::process;
use std::rc::Rc;

fn parse_target(args: &[String]) -> Result<DiffTarget, Option<String>> {
    if args.is_empty() {
        return Ok(DiffTarget::WorkingTree);
    }
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help") {
        return Err(None);
    }
    if args.len() == 1 && (args[0] == "--staged" || args[0] == "--cached") {
        return Ok(DiffTarget::Staged);
    }
    Ok(DiffTarget::Range(args.to_vec()))
}

fn print_usage() {
    println!(
        "\
review — minimal terminal UI for reviewing git changes

USAGE:
    review [OPTIONS | GIT_DIFF_ARGS...]

OPTIONS:
    --staged, --cached    Review staged changes only
    -h, --help            Show this help message

KEYBINDINGS:
    f                     Toggle files panel (shown by default)
    c                     Open config modal (wrap, whitespace, untracked)
    ctrl+w                Toggle whitespace changes (hidden by default)
    ctrl+u                Toggle untracked files (shown by default)
    ctrl+z                Toggle line wrapping (off by default)
    s                     Stage currently viewed file
    u                     Unstage currently selected staged file
    e                     Edit currently viewed file in $EDITOR
    n / p                 Next / previous file (across staged & changed)
    j / k / h / l         Scroll down / up / left / right
    PageUp / PageDown     Scroll half a page
    g / G                 Jump to top / bottom
    R                     Reload git diff
    q / ctrl+c            Quit"
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let target = match parse_target(&args) {
        Ok(target) => target,
        Err(None) => {
            print_usage();
            return;
        }
        Err(Some(msg)) => {
            eprintln!("error: {msg}");
            process::exit(2);
        }
    };

    let cwd = env::current_dir().unwrap_or_else(|err| {
        eprintln!("error: cannot determine current directory: {err}");
        process::exit(1);
    });

    let root = repo_root(&cwd).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    let initial_opts = git::DiffOptions {
        target: target.clone(),
        ..git::DiffOptions::default()
    };

    let loaded = load_diff(&root, &initial_opts).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    let app = App::new(
        AppOptions::default()
            .with_theme(Theme::terminal())
            .with_title("review"),
    )
    .unwrap_or_else(|err| {
        eprintln!("error: failed to initialize terminal UI: {err}");
        process::exit(1);
    });

    let loader_root = root.clone();
    let loader: ui::DiffLoader = Rc::new(move |opts| load_diff(&loader_root, opts));
    let opener_root = root.clone();
    let opener: ui::FileOpener = Rc::new(move |rel_path| open_in_editor(&opener_root, rel_path));
    let stager_root = root.clone();
    let stager: ui::FileStager =
        Rc::new(move |rel_path, old_path| stage_file(&stager_root, rel_path, old_path));
    let unstager_root = root.clone();
    let unstager: ui::FileUnstager =
        Rc::new(move |rel_path, old_path| unstage_file(&unstager_root, rel_path, old_path));
    let initial_state = AppState::new(loaded, target);

    ui::mount(&app, initial_state, loader, opener, stager, unstager);

    if let Err(err) = app.run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn resolve_editor() -> Result<String, String> {
    env::var("EDITOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env::var("VISUAL").ok().filter(|s| !s.trim().is_empty()))
        .ok_or_else(|| "$EDITOR is not set".to_owned())
}

fn run_editor(editor: &str, repo_root: &Path, rel_path: &str) -> Result<(), String> {
    let full_path = repo_root.join(rel_path);
    let status = process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("--")
        .arg(&full_path)
        .current_dir(repo_root)
        .status()
        .map_err(|err| format!("failed to launch editor: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        match status.code() {
            Some(code) => Err(format!("editor exited with status {code}")),
            None => Err("editor terminated by signal".to_owned()),
        }
    }
}

fn open_in_editor(repo_root: &Path, rel_path: &str) -> Result<(), String> {
    let editor = resolve_editor()?;
    run_editor(&editor, repo_root, rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn run_editor_passes_full_path_and_supports_arguments() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("review-editor-test-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("hello world.txt");
        fs::write(&target, "before\n").unwrap();

        // Simulate an $EDITOR with flags that modifies the file passed as "$1".
        run_editor("sh -c 'echo edited >> \"$1\"' --", &dir, "hello world.txt").unwrap();
        let content = fs::read_to_string(&target).unwrap();
        assert_eq!(content, "before\nedited\n");

        // Non-zero exit status is reported as an error.
        let err = run_editor("false", &dir, "hello world.txt").unwrap_err();
        assert!(err.contains("exited with status"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(&dir);
    }
}
