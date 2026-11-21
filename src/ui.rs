use crate::git::{DiffFile, DiffLineKind, DiffOptions, FileStatus};
use crate::state::AppState;
use crate::tree::{TreeRowKind, build_tree_rows};
use opentui::prelude::*;
use opentui::widget::ScrollCause;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const SIDE_PANEL_WIDTH: u16 = 34;

pub type DiffLoader = Rc<dyn Fn(&DiffOptions) -> std::result::Result<Vec<DiffFile>, String>>;

struct SideFileRow {
    widget: Text,
    prefix: String,
    label: String,
    status: FileStatus,
}

struct UiContext {
    app: App,
    state: AppState,
    loader: DiffLoader,
    side_panel: View,
    side_scroll: ScrollBox,
    main_scroll: ScrollBox,
    btn_panel: Text,
    btn_ws: Text,
    btn_untracked: Text,
    btn_review: Text,
    footer_status: Text,
    /// Ordered list of (path, section_view) currently mounted in `main_scroll`.
    section_views: Vec<(String, View)>,
    /// Map of unreviewed file path -> its side panel row for fast highlight updates.
    side_file_widgets: HashMap<String, SideFileRow>,
    /// Last error message from reloading git diff, if any.
    last_error: Option<String>,
    /// File path to scroll to once the next rebuild's layout has been computed.
    pending_scroll_to: Option<String>,
}

/// Mounts the complete reviewer UI onto `app.root()` and binds all keyboard/mouse handlers.
pub fn mount(app: &App, initial_state: AppState, loader: DiffLoader) {
    let root = app.root();
    root.column();

    let body = View::new(app).row().grow(1.0).min_height(0);

    let side_scroll = ScrollBox::new(app)
        .grow(1.0)
        .min_height(0)
        .layout_mode(LayoutMode::List)
        .scrollbars(false);

    let side_panel = View::new(app)
        .column()
        .width(SIDE_PANEL_WIDTH)
        .shrink(0.0)
        .min_height(0)
        .padding_x(1)
        .visible(!initial_state.hide_side_panel)
        .child(&side_scroll);

    let main_scroll = ScrollBox::new(app)
        .grow(1.0)
        .min_width(0)
        .min_height(0)
        .axes(true, true)
        .layout_mode(LayoutMode::List)
        .scrollbars(false);
    // Clear ScrollBox's default keymap so our global vim/navigation keys act consistently.
    main_scroll.set_keymap(Keymap::new());
    side_scroll.set_keymap(Keymap::new());

    body.add(&side_panel);
    body.add(&main_scroll);

    let btn_panel = make_footer_button(app);
    let btn_ws = make_footer_button(app);
    let btn_untracked = make_footer_button(app);
    let btn_review = make_footer_button(app);

    let footer_buttons = View::new(app)
        .row()
        .gap(1)
        .shrink(0.0)
        .child(&btn_panel)
        .child(&btn_ws)
        .child(&btn_untracked)
        .child(&btn_review);

    let footer_status = Text::new(app, "")
        .fg(Color::BRIGHT_BLACK)
        .truncate(true)
        .selectable(false)
        .shrink(1.0)
        .min_width(0);

    let footer = View::new(app)
        .row()
        .height(1)
        .shrink(0.0)
        .padding_x(1)
        .gap(2)
        .justify(Justify::SpaceBetween)
        .child(&footer_buttons)
        .child(&footer_status);

    root.add(&body);
    root.add(&footer);

    let ctx = Rc::new(RefCell::new(UiContext {
        app: app.clone(),
        state: initial_state,
        loader,
        side_panel,
        side_scroll,
        main_scroll: main_scroll.clone(),
        btn_panel: btn_panel.clone(),
        btn_ws: btn_ws.clone(),
        btn_untracked: btn_untracked.clone(),
        btn_review: btn_review.clone(),
        footer_status,
        section_views: Vec::new(),
        side_file_widgets: HashMap::new(),
        last_error: None,
        pending_scroll_to: None,
    }));

    rebuild_all(&ctx);
    main_scroll.focus();

    // Wire footer toggle/action buttons.
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_panel.on_click(move |_cx, _ev| toggle_side_panel(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_ws.on_click(move |_cx, _ev| toggle_whitespace(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_untracked.on_click(move |_cx, _ev| toggle_untracked(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_review.on_click(move |_cx, _ev| mark_current_reviewed(&ctx_btn));
    }

    // Track currently viewed file as the user scrolls the main diff panel.
    {
        let ctx_scroll = Rc::clone(&ctx);
        main_scroll.on_scroll(move |_cx, event| {
            // Keep explicit selection intact on programmatic jumps (e.g., clicking a short last file).
            if event.cause == ScrollCause::Program {
                return;
            }
            sync_active_file_from_scroll(&ctx_scroll);
        });
    }

    // Global keybindings.
    {
        let ctx_key = Rc::clone(&ctx);
        app.on_key(move |cx, key| {
            handle_key(cx, key, &ctx_key);
        });
    }
}

fn make_footer_button(app: &App) -> Text {
    Text::new(app, "")
        .selectable(false)
        .shrink(0.0)
        .mouse_pointer(MousePointer::POINTER)
}

fn toggle_side_panel(ctx: &Rc<RefCell<UiContext>>) {
    let mut inner = ctx.borrow_mut();
    inner.state.hide_side_panel = !inner.state.hide_side_panel;
    let visible = !inner.state.hide_side_panel;
    inner.side_panel.visible(visible);
    update_footer(&inner);
}

fn toggle_untracked(ctx: &Rc<RefCell<UiContext>>) {
    {
        let mut inner = ctx.borrow_mut();
        inner.state.hide_untracked = !inner.state.hide_untracked;
    }
    reload_from_git(ctx);
}

fn toggle_whitespace(ctx: &Rc<RefCell<UiContext>>) {
    {
        let mut inner = ctx.borrow_mut();
        inner.state.hide_whitespace = !inner.state.hide_whitespace;
    }
    reload_from_git(ctx);
}

fn mark_current_reviewed(ctx: &Rc<RefCell<UiContext>>) {
    let changed = {
        let mut inner = ctx.borrow_mut();
        inner.state.mark_active_reviewed().is_some()
    };
    if changed {
        let next_target = ctx.borrow().state.active_file.clone();
        ctx.borrow_mut().pending_scroll_to = next_target.clone();
        rebuild_all(ctx);
        if next_target.is_none() {
            ctx.borrow().main_scroll.scroll_to(0.0, 0.0);
        }
    }
}

fn handle_key(cx: &mut Cx<'_>, key: &KeyEvent, ctx: &Rc<RefCell<UiContext>>) {
    if key.matches("q") || key.matches("ctrl+c") {
        cx.quit();
        return;
    }

    if key.matches("ctrl+p") {
        toggle_side_panel(ctx);
        return;
    }

    if key.matches("ctrl+u") {
        toggle_untracked(ctx);
        return;
    }

    if key.matches("ctrl+w") {
        toggle_whitespace(ctx);
        return;
    }

    if key.matches("r") {
        mark_current_reviewed(ctx);
        return;
    }

    if key.matches("R") {
        reload_from_git(ctx);
        return;
    }

    if key.matches("n") {
        let target = ctx.borrow().state.next_active_file(1);
        if let Some(path) = target {
            update_active_highlight(ctx, Some(path.clone()));
            scroll_to_file(ctx, &path);
        }
        return;
    }

    if key.matches("p") {
        let target = ctx.borrow().state.next_active_file(-1);
        if let Some(path) = target {
            update_active_highlight(ctx, Some(path.clone()));
            scroll_to_file(ctx, &path);
        }
        return;
    }

    // Vim & standard scrolling in the main diff view.
    let main = ctx.borrow().main_scroll.clone();
    let page = (f64::from(main.rect().height) / 2.0).max(1.0).floor();

    if key.matches("j") || key.matches("down") {
        main.scroll_by(0.0, 1.0);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("k") || key.matches("up") {
        main.scroll_by(0.0, -1.0);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("h") || key.matches("left") {
        main.scroll_by(-4.0, 0.0);
    } else if key.matches("l") || key.matches("right") {
        main.scroll_by(4.0, 0.0);
    } else if key.matches("pagedown") || key.matches("space") || key.matches("ctrl+d") {
        main.scroll_by(0.0, page);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("pageup") {
        main.scroll_by(0.0, -page);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("g") || key.matches("home") {
        main.scroll_to(0.0, 0.0);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("G") || key.matches("end") {
        main.scroll_to_bottom();
        sync_active_file_from_scroll(ctx);
    }
}

fn reload_from_git(ctx: &Rc<RefCell<UiContext>>) {
    let (loader, opts) = {
        let inner = ctx.borrow();
        (Rc::clone(&inner.loader), inner.state.diff_options())
    };
    match loader(&opts) {
        Ok(files) => {
            let target_file = {
                let mut inner = ctx.borrow_mut();
                inner.last_error = None;
                inner.state.reload_files(files);
                let active = inner.state.active_file.clone();
                inner.pending_scroll_to = active.clone();
                active
            };
            rebuild_all(ctx);
            if target_file.is_none() {
                ctx.borrow().main_scroll.scroll_to(0.0, 0.0);
            }
        }
        Err(err) => {
            let mut inner = ctx.borrow_mut();
            inner.last_error = Some(err);
            update_footer(&inner);
        }
    }
}

fn rebuild_all(ctx: &Rc<RefCell<UiContext>>) {
    rebuild_side_panel(ctx);
    rebuild_main_panel(ctx);
    let inner = ctx.borrow();
    update_footer(&inner);
}

fn rebuild_side_panel(ctx: &Rc<RefCell<UiContext>>) {
    let (app, side_scroll, unreviewed_rows, reviewed_rows, active_file, unreviewed_count, reviewed_count) = {
        let inner = ctx.borrow();
        let unreviewed = inner.state.unreviewed_files();
        let reviewed = inner.state.reviewed_files();
        let u_rows = build_tree_rows(&unreviewed, &inner.state.collapsed_dirs);
        let r_rows = build_tree_rows(&reviewed, &inner.state.collapsed_dirs);
        (
            inner.app.clone(),
            inner.side_scroll.clone(),
            u_rows,
            r_rows,
            inner.state.active_file.clone(),
            unreviewed.len(),
            reviewed.len(),
        )
    };

    side_scroll.clear();
    let mut file_widgets = HashMap::new();

    // Unreviewed section header.
    let changed_header = Text::new(&app, format!("Changed ({unreviewed_count})"))
        .fg(Color::DEFAULT_FOREGROUND)
        .bold()
        .selectable(false);
    side_scroll.add(&changed_header);

    if unreviewed_rows.is_empty() {
        let empty = Text::new(&app, "  (none)")
            .fg(Color::BRIGHT_BLACK)
            .selectable(false);
        side_scroll.add(&empty);
    } else {
        for row in unreviewed_rows {
            match row.kind {
                TreeRowKind::Dir { path, .. } => {
                    let text = Text::new(&app, format!("{}{}", row.prefix, row.label))
                        .fg(Color::CYAN)
                        .truncate(true)
                        .selectable(false)
                        .mouse_pointer(MousePointer::POINTER);
                    let ctx_click = Rc::clone(ctx);
                    text.on_click(move |_cx, _ev| {
                        ctx_click.borrow_mut().state.toggle_dir(&path);
                        rebuild_side_panel(&ctx_click);
                    });
                    side_scroll.add(&text);
                }
                TreeRowKind::File {
                    path,
                    status,
                    ..
                } => {
                    let is_active = active_file.as_deref() == Some(path.as_str());
                    let text = build_side_file_row(&app, &row.prefix, &row.label, status, is_active);
                    let ctx_click = Rc::clone(ctx);
                    let click_path = path.clone();
                    text.on_click(move |_cx, _ev| {
                        update_active_highlight(&ctx_click, Some(click_path.clone()));
                        scroll_to_file(&ctx_click, &click_path);
                    });
                    file_widgets.insert(
                        path,
                        SideFileRow {
                            widget: text.clone(),
                            prefix: row.prefix,
                            label: row.label,
                            status,
                        },
                    );
                    side_scroll.add(&text);
                }
            }
        }
    }

    // Reviewed section (shown when at least one file has been marked reviewed).
    if reviewed_count > 0 {
        let divider = Text::new(&app, format!("\n── Reviewed ({reviewed_count}) ──"))
            .fg(Color::BRIGHT_BLACK)
            .selectable(false);
        side_scroll.add(&divider);

        for row in reviewed_rows {
            match row.kind {
                TreeRowKind::Dir { path, .. } => {
                    let text = Text::new(&app, format!("{}{}", row.prefix, row.label))
                        .fg(Color::BRIGHT_BLACK)
                        .truncate(true)
                        .selectable(false)
                        .mouse_pointer(MousePointer::POINTER);
                    let ctx_click = Rc::clone(ctx);
                    text.on_click(move |_cx, _ev| {
                        ctx_click.borrow_mut().state.toggle_dir(&path);
                        rebuild_side_panel(&ctx_click);
                    });
                    side_scroll.add(&text);
                }
                TreeRowKind::File { path, .. } => {
                    let text = Text::new(&app, format!("{}✓ {}", row.prefix, row.label))
                        .fg(Color::BRIGHT_BLACK)
                        .truncate(true)
                        .selectable(false)
                        .mouse_pointer(MousePointer::POINTER);
                    let ctx_click = Rc::clone(ctx);
                    let click_path = path.clone();
                    text.on_click(move |_cx, _ev| {
                        let changed = ctx_click.borrow_mut().state.unmark_reviewed(&click_path);
                        if changed {
                            ctx_click.borrow_mut().pending_scroll_to = Some(click_path.clone());
                            rebuild_all(&ctx_click);
                        }
                    });
                    side_scroll.add(&text);
                }
            }
        }
    }

    ctx.borrow_mut().side_file_widgets = file_widgets;
}

fn build_side_file_spans(
    prefix: &str,
    label: &str,
    status: FileStatus,
    active: bool,
) -> Vec<Span<'static>> {
    if active {
        vec![
            Span::styled(prefix.to_owned(), Style::new().fg(Color::BRIGHT_BLACK)),
            Span::styled(
                format!(" {} {label} ", status.badge()),
                Style::new().fg(Color::DEFAULT_FOREGROUND).bold().inverse(),
            ),
        ]
    } else {
        vec![
            Span::styled(prefix.to_owned(), Style::new().fg(Color::BRIGHT_BLACK)),
            Span::styled(
                format!("{} ", status.badge()),
                Style::new().fg(status_color(status)),
            ),
            Span::styled(label.to_owned(), Style::new().fg(Color::DEFAULT_FOREGROUND)),
        ]
    }
}

fn build_side_file_row(
    app: &App,
    prefix: &str,
    label: &str,
    status: FileStatus,
    active: bool,
) -> Text {
    Text::new(app, build_side_file_spans(prefix, label, status, active))
        .truncate(true)
        .selectable(false)
        .mouse_pointer(MousePointer::POINTER)
}

fn rebuild_main_panel(ctx: &Rc<RefCell<UiContext>>) {
    let (app, main_scroll, unreviewed, total, pending_target) = {
        let mut inner = ctx.borrow_mut();
        let unreviewed: Vec<DiffFile> = inner
            .state
            .unreviewed_files()
            .into_iter()
            .cloned()
            .collect();
        (
            inner.app.clone(),
            inner.main_scroll.clone(),
            unreviewed,
            inner.state.total_count(),
            inner.pending_scroll_to.take(),
        )
    };

    main_scroll.clear();
    let mut section_views = Vec::with_capacity(unreviewed.len());

    if unreviewed.is_empty() {
        let message = if total == 0 {
            "No git changes."
        } else {
            "All files marked as reviewed. Open side panel (ctrl+p) to un-review."
        };
        let empty = View::new(&app)
            .padding(2)
            .child(&Text::new(&app, message).fg(Color::BRIGHT_BLACK));
        main_scroll.add(&empty);
    } else {
        for file in &unreviewed {
            let section = build_file_section(&app, file);
            if pending_target.as_deref() == Some(file.path.as_str()) {
                let scroll = main_scroll.clone();
                let applied = std::cell::Cell::new(false);
                section.on_resize(move |_cx, rect| {
                    if !applied.replace(true) {
                        scroll.scroll_to(0.0, f64::from(rect.top).max(0.0));
                    }
                });
            }
            main_scroll.add(&section);
            section_views.push((file.path.clone(), section));
        }
    }

    ctx.borrow_mut().section_views = section_views;
}

fn build_file_section(app: &App, file: &DiffFile) -> View {
    let status_fg = status_color(file.status);
    let mut header_spans = vec![
        Span::styled(
            format!("{} ", file.status.badge()),
            Style::new().fg(status_fg).bold(),
        ),
        Span::styled(
            file.display_header_path(),
            Style::new().fg(Color::DEFAULT_FOREGROUND).bold(),
        ),
    ];

    if file.additions > 0 || file.deletions > 0 {
        header_spans.push(Span::raw("  "));
        if file.additions > 0 {
            header_spans.push(Span::styled(
                format!("+{}", file.additions),
                Style::new().fg(Color::GREEN),
            ));
        }
        if file.additions > 0 && file.deletions > 0 {
            header_spans.push(Span::raw(" "));
        }
        if file.deletions > 0 {
            header_spans.push(Span::styled(
                format!("-{}", file.deletions),
                Style::new().fg(Color::RED),
            ));
        }
    }

    let header = View::new(app)
        .border_sides(Sides::BOTTOM)
        .border_color(Color::BRIGHT_BLACK)
        .padding_x(1)
        .child(&Text::new(app, header_spans).wrap(WrapMode::None));

    let mut diff_spans = Vec::with_capacity(file.lines.len() * 2);
    for (idx, line) in file.lines.iter().enumerate() {
        if idx > 0 {
            diff_spans.push(Span::raw("\n"));
        }
        let style = match line.kind {
            DiffLineKind::HunkHeader => Style::new().fg(Color::CYAN),
            DiffLineKind::Added => Style::new().fg(Color::GREEN),
            DiffLineKind::Removed => Style::new().fg(Color::RED),
            DiffLineKind::Context => Style::new().fg(Color::DEFAULT_FOREGROUND),
            DiffLineKind::Notice => Style::new().fg(Color::BRIGHT_BLACK),
        };
        diff_spans.push(Span::styled(line.text.clone(), style));
    }

    let body = View::new(app)
        .padding_x(1)
        .child(&Text::new(app, diff_spans).wrap(WrapMode::None));

    View::new(app)
        .column()
        .border(BorderStyle::Rounded)
        .border_color(Color::BRIGHT_BLACK)
        .margin_x(1)
        .margin_edge(Edge::Bottom, 1)
        .child(&header)
        .child(&body)
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Added | FileStatus::Untracked => Color::GREEN,
        FileStatus::Deleted => Color::RED,
        FileStatus::Modified => Color::YELLOW,
        FileStatus::Renamed => Color::CYAN,
    }
}

fn scroll_to_file(ctx: &Rc<RefCell<UiContext>>, path: &str) {
    let inner = ctx.borrow();
    if let Some((_, view)) = inner.section_views.iter().find(|(p, _)| p == path) {
        let target_y = f64::from(view.rect().top).max(0.0);
        inner.main_scroll.scroll_to(0.0, target_y);
    }
}

fn sync_active_file_from_scroll(ctx: &Rc<RefCell<UiContext>>) {
    let detected = {
        let inner = ctx.borrow();
        let viewport_top = inner.main_scroll.rect().screen_y;
        let mut active: Option<String> = inner.section_views.first().map(|(p, _)| p.clone());
        for (path, view) in &inner.section_views {
            let rel_y = view.rect().screen_y - viewport_top;
            if rel_y <= 1.0 {
                active = Some(path.clone());
            } else {
                break;
            }
        }
        active
    };

    if detected != ctx.borrow().state.active_file {
        update_active_highlight(ctx, detected);
    }
}

fn update_active_highlight(ctx: &Rc<RefCell<UiContext>>, new_active: Option<String>) {
    let mut inner = ctx.borrow_mut();
    let old_active = inner.state.active_file.clone();
    if old_active == new_active {
        return;
    }
    if let Some(old) = old_active.as_deref()
        && let Some(row) = inner.side_file_widgets.get(old)
    {
        row.widget
            .set_text(build_side_file_spans(&row.prefix, &row.label, row.status, false));
    }
    if let Some(new) = new_active.as_deref()
        && let Some(row) = inner.side_file_widgets.get(new)
    {
        row.widget
            .set_text(build_side_file_spans(&row.prefix, &row.label, row.status, true));
    }
    inner.state.active_file = new_active;
    update_footer(&inner);
}

fn style_toggle_button(button: &Text, label: &str, active: bool) {
    if active {
        button
            .set_text(format!(" {label} "))
            .fg(Color::DEFAULT_FOREGROUND)
            .bg(Color::TRANSPARENT)
            .attributes(Attributes::BOLD | Attributes::INVERSE);
    } else {
        button
            .set_text(format!(" {label} "))
            .fg(Color::WHITE)
            .bg(Color::BRIGHT_BLACK)
            .attributes(Attributes::NONE);
    }
}

fn update_footer(inner: &UiContext) {
    let panel_on = !inner.state.hide_side_panel;
    let ws_on = !inner.state.hide_whitespace;
    let untracked_on = !inner.state.hide_untracked;

    style_toggle_button(
        &inner.btn_panel,
        &format!("^p panel:{}", if panel_on { "on" } else { "off" }),
        panel_on,
    );
    style_toggle_button(
        &inner.btn_ws,
        &format!("^w ws:{}", if ws_on { "on" } else { "off" }),
        ws_on,
    );
    style_toggle_button(
        &inner.btn_untracked,
        &format!("^u untracked:{}", if untracked_on { "on" } else { "off" }),
        untracked_on,
    );

    if inner.state.active_file.is_some() {
        inner
            .btn_review
            .set_text(" r review ")
            .fg(Color::BLACK)
            .bg(Color::GREEN)
            .attributes(Attributes::BOLD);
    } else {
        inner
            .btn_review
            .set_text(" r review ")
            .fg(Color::WHITE)
            .bg(Color::BRIGHT_BLACK)
            .attributes(Attributes::DIM);
    }

    if let Some(err) = &inner.last_error {
        inner.footer_status.set_text(format!("error: {err}")).fg(Color::RED);
    } else {
        inner
            .footer_status
            .set_text(format!(
                "{}/{} reviewed",
                inner.state.reviewed_count(),
                inner.state.total_count()
            ))
            .fg(Color::BRIGHT_BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, DiffTarget};
    use opentui::testing::TestApp;

    fn sample_files(opts: &DiffOptions) -> Vec<DiffFile> {
        let mut files = vec![
            DiffFile {
                path: "src/alpha.rs".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 1,
                deletions: 1,
                is_binary: false,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::HunkHeader,
                        text: "@@ -1,2 +1,2 @@".to_owned(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        text: "-old alpha".to_owned(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        text: "+new alpha".to_owned(),
                    },
                ],
            },
            DiffFile {
                path: "src/beta.rs".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::HunkHeader,
                        text: "@@ -1,1 +1,2 @@".to_owned(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        text: "+beta change".to_owned(),
                    },
                ],
            },
        ];

        if !opts.hide_whitespace {
            files.push(DiffFile {
                path: "ws_only.txt".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 1,
                deletions: 1,
                is_binary: false,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    text: "+  whitespace".to_owned(),
                }],
            });
        }

        if !opts.hide_untracked {
            files.push(DiffFile {
                path: "untracked.txt".to_owned(),
                old_path: None,
                status: FileStatus::Untracked,
                additions: 1,
                deletions: 0,
                is_binary: false,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    text: "+fresh file".to_owned(),
                }],
            });
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    fn capture_text(capture: &opentui::testing::Capture) -> String {
        (0..capture.height)
            .map(|row| capture.row(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn setup_test_app() -> TestApp {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let state = AppState::new(sample_files(&DiffOptions::default()), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(|opts| Ok(sample_files(opts)));
        mount(&app, state, loader);
        app
    }

    #[test]
    fn toggles_side_panel_whitespace_and_untracked() {
        let app = setup_test_app();

        // Initial state: side panel shown, whitespace hidden, untracked shown.
        let screen = capture_text(&app.render());
        assert!(screen.contains("Changed (3)"));
        assert!(screen.contains("untracked.txt"));
        assert!(!screen.contains("ws_only.txt"));

        // ctrl+p hides side panel, and pressing again restores it.
        app.press("ctrl+p");
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Changed (3)"));
        app.press("ctrl+p");
        let screen = capture_text(&app.render());
        assert!(screen.contains("Changed (3)"));

        // ctrl+w shows whitespace changes.
        app.press("ctrl+w");
        let screen = capture_text(&app.render());
        assert!(screen.contains("ws_only.txt"));
        assert!(screen.contains("Changed (4)"));

        // ctrl+u hides untracked files.
        app.press("ctrl+u");
        let screen = capture_text(&app.render());
        assert!(!screen.contains("untracked.txt"));
        assert!(screen.contains("Changed (3)"));
    }

    #[test]
    fn mark_reviewed_removes_from_main_and_moves_to_reviewed_section() {
        let app = setup_test_app();
        let before = capture_text(&app.render());
        assert!(before.contains("+new alpha"));
        assert!(!before.contains("Reviewed (1)"));

        // Mark active file (src/alpha.rs) as reviewed.
        app.press("r");
        let after = capture_text(&app.render());
        assert!(!after.contains("+new alpha"));
        assert!(after.contains("Reviewed (1)"));
        assert!(after.contains("✓ alpha.rs"));
        assert!(after.contains("1/3 reviewed"));
    }

    #[test]
    fn mouse_click_in_side_panel_unreviews_file() {
        let app = setup_test_app();
        app.press("r");
        let capture = app.render();

        // Find the row containing `✓ alpha.rs` in the side panel and click it.
        let reviewed_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("✓ alpha.rs"))
            .expect("reviewed row should be visible in side panel");

        app.click(4, reviewed_row as u16);
        let restored = capture_text(&app.render());
        assert!(restored.contains("+new alpha"));
        assert!(!restored.contains("Reviewed (1)"));
    }

    #[test]
    fn next_prev_and_vim_scroll_keys_work() {
        let app = setup_test_app();
        let screen = capture_text(&app.render());
        assert!(screen.contains(" M alpha.rs "));

        app.press("n");
        let screen = capture_text(&app.render());
        assert!(screen.contains(" M beta.rs "));

        app.press("p");
        let screen = capture_text(&app.render());
        assert!(screen.contains(" M alpha.rs "));

        // Vim j/k scrolling should not panic or break rendering.
        app.press("j");
        app.press("j");
        app.press("k");
        let _ = app.render();
    }

    #[test]
    fn mouse_click_selects_unreviewed_file_in_side_panel() {
        let app = setup_test_app();
        let capture = app.render();

        let beta_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("beta.rs"))
            .expect("beta.rs should be visible in side panel");

        app.click(4, beta_row as u16);
        let screen = capture_text(&app.render());
        assert!(screen.contains(" M beta.rs "));
    }

    #[test]
    fn toggling_whitespace_stays_on_current_file() {
        let app = TestApp::with_options(
            120,
            12,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let make_tall_files = |opts: &DiffOptions| {
            let mut alpha_lines = vec![DiffLine {
                kind: DiffLineKind::HunkHeader,
                text: "@@ -1,20 +1,20 @@".to_owned(),
            }];
            for i in 0..20 {
                alpha_lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    text: format!("+alpha line {i}"),
                });
            }
            let mut beta_lines = vec![DiffLine {
                kind: DiffLineKind::HunkHeader,
                text: "@@ -1,15 +1,15 @@".to_owned(),
            }];
            if !opts.hide_whitespace {
                beta_lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    text: "+  whitespace change in beta".to_owned(),
                });
            }
            for i in 0..15 {
                beta_lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    text: format!("+beta line {i}"),
                });
            }
            vec![
                DiffFile {
                    path: "src/alpha.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Modified,
                    additions: 20,
                    deletions: 0,
                    is_binary: false,
                    lines: alpha_lines,
                },
                DiffFile {
                    path: "src/beta.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Modified,
                    additions: beta_lines.len() - 1,
                    deletions: 0,
                    is_binary: false,
                    lines: beta_lines,
                },
            ]
        };

        let state = AppState::new(make_tall_files(&DiffOptions::default()), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |opts| Ok(make_tall_files(opts)));
        mount(&app, state, loader);
        let _ = app.render();

        // Jump to second file (src/beta.rs), which is well below the first viewport.
        app.press("n");
        let before = capture_text(&app.render());
        assert!(before.contains(" M beta.rs "));
        assert!(before.contains("+beta line 0"));

        // Toggle whitespace on: should stay on src/beta.rs and show its lines.
        app.press("ctrl+w");
        let _ = app.render();
        let after = capture_text(&app.render());
        assert!(after.contains(" M beta.rs "));
        assert!(after.contains("+  whitespace change in beta"));
    }

    #[test]
    fn clicking_footer_buttons_toggles_panel_whitespace_untracked_and_review() {
        let app = setup_test_app();
        let capture = app.render();
        let footer_y = (capture.height - 1) as u16;
        let footer_row = capture.row(capture.height - 1);

        // Panel button is at the left of the footer.
        let panel_x = footer_row.find(" ^p panel:on ").expect("panel button") as u16 + 2;
        app.click(panel_x, footer_y);
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Changed (3)"));
        assert!(screen.contains(" ^p panel:off "));

        // Click again to bring side panel back.
        app.click(panel_x, footer_y);
        let capture = app.render();
        let footer_row = capture.row(capture.height - 1);
        assert!(footer_row.contains(" ^p panel:on "));

        // Click whitespace button.
        let ws_x = footer_row.find(" ^w ws:off ").expect("ws button") as u16 + 2;
        app.click(ws_x, footer_y);
        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(screen.contains("ws_only.txt"));
        assert!(screen.contains(" ^w ws:on "));

        // Click untracked button.
        let footer_row = capture.row(capture.height - 1);
        let untracked_x = footer_row.find(" ^u untracked:on ").expect("untracked button") as u16 + 2;
        app.click(untracked_x, footer_y);
        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(!screen.contains("untracked.txt"));
        assert!(screen.contains(" ^u untracked:off "));

        // Click review button.
        let footer_row = capture.row(capture.height - 1);
        let review_x = footer_row.find(" r review ").expect("review button") as u16 + 2;
        app.click(review_x, footer_y);
        let screen = capture_text(&app.render());
        assert!(screen.contains("Reviewed (1)"));
        assert!(screen.contains("1/3 reviewed"));
    }
}
