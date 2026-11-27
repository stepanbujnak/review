# review

A minimal terminal UI utility for reviewing Git changes, built with Rust and [OpenTUI](https://github.com/anomalyco/opentui).

## Features

- **Continuous diff view** — Browse all unstaged changes in a single scrollable view with color-coded unified diffs, rename detection (`-M`), and untracked file previews (up to 2 MiB).
- **Interactive file tree** — Collapsible directory trees in the side panel with a **Staged** section at the top and **Changed** section below, complete with status badges (`M`, `A`, `D`, `R`, `?`, plus two-letter indicators like `MM` / `AM` when a staged file has newer working-tree updates), synced with your current scroll position.
- **Stage workflow** — Press `s` to stage the active file in Git, moving it to the Staged section at the top of the sidebar and automatically advancing to the next changed file. Select any staged file (by clicking it or navigating with `n`/`p`) to inspect its staged diff and press `u` to unstage it. When no staged files remain, the Staged section disappears.
- **Sensible defaults, instant toggles** — Files panel shown, whitespace changes hidden (`git diff -w`), and untracked files included by default. Toggle any of them on the fly via keyboard shortcuts or the `c config` modal.
- **Flexible diff targets** — Review working tree changes (including repositories with no commits yet), staged changes, or any `git diff` revision/range.
- **Live reload** — Reload diffs and staged status from Git at any time.

## Prerequisites

- Rust toolchain (Edition 2024)
- `git`
- `curl` and `unzip` (to fetch the prebuilt OpenTUI native library)
- Supported platforms: macOS (`arm64`, `x86_64`) and Linux (`x86_64`, `arm64`)

## Building & Running

1. Download the OpenTUI native library into `vendor/opentui-native` (configured automatically via `.cargo/config.toml`):

   ```bash
   ./scripts/fetch-opentui-native.sh
   ```

2. Build or run with Cargo:

   ```bash
   cargo build --release
   ```

   Or install it onto your `PATH`:

   ```bash
   cargo install --path .
   ```

## Usage

```bash
review [OPTIONS | GIT_DIFF_ARGS...]
```

### Examples

```bash
# Review working tree changes against HEAD (plus untracked files)
review

# Review staged changes only
review --staged
review --cached

# Review a commit range or branch comparison
review main..HEAD
review HEAD~3..HEAD

# Show CLI usage and keybindings
review --help
```

## Keybindings

| Key | Action |
| --- | --- |
| `f` | Toggle files panel (shown by default) |
| `c` | Open config modal (wrap, whitespace, untracked) |
| `ctrl+w` | Toggle whitespace changes (hidden by default) |
| `ctrl+u` | Toggle untracked files (shown by default) |
| `ctrl+z` | Toggle line wrapping (off by default) |
| `s` | Stage currently viewed file (`git add`) |
| `u` | Unstage currently selected staged file |
| `e` | Edit currently viewed file in `$EDITOR` |
| `n` / `p` | Jump to next / previous file (across staged & changed) |
| `j` / `k` or `↓` / `↑` | Scroll down / up |
| `h` / `l` or `←` / `→` | Scroll left / right |
| `PageDown` / `Space` / `ctrl+d` | Scroll down half a page |
| `PageUp` | Scroll up half a page |
| `g` / `Home` | Jump to top |
| `G` / `End` | Jump to bottom |
| `r` / `R` | Refresh git diff and staged status |
| `q` / `ctrl+c` | Quit |

### Mouse Controls

- **Staged & changed files**: Click any file in the side panel to highlight it and view its diff.
- **Directories**: Click a directory row to collapse or expand it.
- **Footer bar**: Click `s stage` / `u unstage`, `e edit`, `f files`, `c config`, or `r refresh` to trigger the corresponding action.
- **Config modal**: Click any option (`Wrap lines`, `Whitespace changes`, `Untracked files`) to toggle it, or click outside the modal to close it.

## Development

Run the test suite (unit and integration tests covering Git diff parsing, tree layout, state management, and UI interactions):

```bash
cargo test
```
