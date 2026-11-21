# review

A minimal terminal UI utility for reviewing Git changes, built with Rust and [OpenTUI](https://github.com/anomalyco/opentui).

## Features

- **Continuous diff view** — Browse all unreviewed changes in a single scrollable view with color-coded unified diffs, rename detection (`-M`), and untracked file previews (up to 2 MiB).
- **Interactive file tree** — Collapsible directory tree in the side panel with status badges (`M`, `A`, `D`, `R`, `?`), synced with your current scroll position.
- **Review workflow** — Mark files as reviewed to move them out of the main diff view and automatically advance to the next unreviewed file. Click any reviewed file in the side panel to bring it back.
- **Sensible defaults, instant toggles** — Side panel shown, whitespace changes hidden (`git diff -w`), and untracked files included by default. Toggle any of them on the fly via keyboard or clickable footer buttons.
- **Flexible diff targets** — Review working tree changes (including repositories with no commits yet), staged changes, or any `git diff` revision/range.
- **Live reload** — Reload diffs from Git without losing your reviewed-files checklist.

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
| `ctrl+p` | Toggle side panel (shown by default) |
| `ctrl+w` | Toggle whitespace changes (hidden by default) |
| `ctrl+u` | Toggle untracked files (shown by default) |
| `ctrl+z` | Toggle line wrapping (off by default) |
| `r` | Mark currently viewed file as reviewed |
| `n` / `p` | Jump to next / previous unreviewed file |
| `j` / `k` or `↓` / `↑` | Scroll down / up |
| `h` / `l` or `←` / `→` | Scroll left / right |
| `PageDown` / `Space` / `ctrl+d` | Scroll down half a page |
| `PageUp` | Scroll up half a page |
| `g` / `Home` | Jump to top |
| `G` / `End` | Jump to bottom |
| `R` | Reload git diff (preserves reviewed state) |
| `q` / `ctrl+c` | Quit |

### Mouse Controls

- **Changed files**: Click a file in the side panel to jump to its diff.
- **Directories**: Click a directory row to collapse or expand it.
- **Reviewed files**: Click a reviewed file in the side panel to un-review it and jump back to it.
- **Footer bar**: Click `^p panel`, `^z wrap`, `^w ws`, `^u untracked`, or `r review` to trigger the corresponding action.

## Development

Run the test suite (unit and integration tests covering Git diff parsing, tree layout, state management, and UI interactions):

```bash
cargo test
```
