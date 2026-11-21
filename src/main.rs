mod git;
mod state;
mod tree;
mod ui;

use crate::git::{DiffTarget, load_diff, repo_root};
use crate::state::AppState;
use opentui::prelude::*;
use std::env;
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
    ctrl+p                Toggle side panel (shown by default)
    ctrl+w                Toggle whitespace changes (hidden by default)
    ctrl+u                Toggle untracked files (shown by default)
    ctrl+z                Toggle line wrapping (off by default)
    r                     Mark currently viewed file as reviewed
    n / p                 Next / previous unreviewed file
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

    let files = load_diff(&root, &initial_opts).unwrap_or_else(|err| {
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
    let initial_state = AppState::new(files, target);

    ui::mount(&app, initial_state, loader);

    if let Err(err) = app.run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
