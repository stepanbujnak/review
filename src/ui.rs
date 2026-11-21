use crate::git::{DiffFile, DiffLineKind, DiffOptions, FileStatus};
use crate::state::AppState;
use crate::tree::{TreeRowKind, build_tree_rows};
use opentui::prelude::*;
use opentui::widget::ScrollCause;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SIDE_PANEL_WIDTH: u16 = 34;
/// Suppresses stray cross-axis trackpad ticks right after scrolling on the other axis.
const AXIS_LOCK_DURATION: Duration = Duration::from_millis(120);
/// Pause after which a horizontal trackpad gesture resets its deadzone.
const HORIZONTAL_GESTURE_TIMEOUT: Duration = Duration::from_millis(200);
/// Consecutive horizontal ticks required before a trackpad swipe begins scrolling.
const HORIZONTAL_SCROLL_DEADZONE: u8 = 3;

#[derive(Default)]
struct WheelAxisFilter {
    last_vertical: Option<Instant>,
    last_horizontal_active: Option<Instant>,
    last_horizontal_raw: Option<Instant>,
    h_direction: i8,
    h_ticks: u8,
}

impl WheelAxisFilter {
    /// Returns `true` if a non-Shift vertical wheel tick should be allowed,
    /// or `false` if suppressed because the user is actively side-scrolling.
    fn allow_vertical(&mut self, now: Instant) -> bool {
        if self
            .last_horizontal_active
            .is_some_and(|t| now.saturating_duration_since(t) < AXIS_LOCK_DURATION)
        {
            return false;
        }
        self.last_vertical = Some(now);
        self.h_direction = 0;
        self.h_ticks = 0;
        true
    }

    /// Returns the horizontal column delta to apply for a non-Shift horizontal wheel tick,
    /// or `None` if suppressed by the vertical-scroll lock or deadzone.
    fn filter_horizontal(&mut self, now: Instant, dir: i8) -> Option<f64> {
        if self
            .last_vertical
            .is_some_and(|t| now.saturating_duration_since(t) < AXIS_LOCK_DURATION)
        {
            self.h_direction = 0;
            self.h_ticks = 0;
            return None;
        }

        let timed_out = self
            .last_horizontal_raw
            .is_none_or(|t| now.saturating_duration_since(t) > HORIZONTAL_GESTURE_TIMEOUT);
        self.last_horizontal_raw = Some(now);

        if timed_out || self.h_direction != dir {
            self.h_direction = dir;
            self.h_ticks = 1;
        } else {
            self.h_ticks = self.h_ticks.saturating_add(1);
        }

        if self.h_ticks < HORIZONTAL_SCROLL_DEADZONE {
            return None;
        }

        self.last_horizontal_active = Some(now);
        Some(f64::from(dir))
    }
}

pub type DiffLoader = Rc<dyn Fn(&DiffOptions) -> std::result::Result<Vec<DiffFile>, String>>;

struct SideFileRow {
    widget: Text,
    prefix: String,
    label: String,
    status: FileStatus,
}

struct FileSection {
    path: String,
    view: View,
    header_text: Text,
    header_width: u32,
    body_text: Text,
    body_width: u32,
    scroll_x: f64,
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
    /// Ordered list of file sections currently mounted in `main_scroll`.
    sections: Vec<FileSection>,
    /// Filters accidental diagonal cross-axis trackpad ticks while scrolling.
    wheel_filter: WheelAxisFilter,
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
        .axes(false, true)
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
        sections: Vec::new(),
        wheel_filter: WheelAxisFilter::default(),
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

    // Horizontal mouse wheel / trackpad scroll inside the hovered (or active) file border,
    // with symmetric axis locking to prevent accidental diagonal cross-axis scrolling.
    {
        let ctx_mouse = Rc::clone(&ctx);
        main_scroll.on_mouse(move |cx, event| {
            let shift = event.modifiers.contains(Modifiers::SHIFT);
            match (event.kind, shift) {
                (MouseEventKind::ScrollUp | MouseEventKind::ScrollDown, false) => {
                    if !ctx_mouse.borrow_mut().wheel_filter.allow_vertical(cx.now()) {
                        cx.prevent_default();
                    }
                }
                (MouseEventKind::ScrollLeft, false) => {
                    let dx = ctx_mouse.borrow_mut().wheel_filter.filter_horizontal(cx.now(), -1);
                    if let Some(dx) = dx {
                        scroll_horizontal_at(&ctx_mouse, event.x, event.y, dx);
                    }
                    cx.prevent_default();
                }
                (MouseEventKind::ScrollRight, false) => {
                    let dx = ctx_mouse.borrow_mut().wheel_filter.filter_horizontal(cx.now(), 1);
                    if let Some(dx) = dx {
                        scroll_horizontal_at(&ctx_mouse, event.x, event.y, dx);
                    }
                    cx.prevent_default();
                }
                (MouseEventKind::ScrollUp, true) => {
                    scroll_horizontal_at(&ctx_mouse, event.x, event.y, -2.0);
                    cx.prevent_default();
                }
                (MouseEventKind::ScrollDown, true) => {
                    scroll_horizontal_at(&ctx_mouse, event.x, event.y, 2.0);
                    cx.prevent_default();
                }
                _ => {}
            }
        });
    }

    // Re-clamp horizontal text scroll when the main viewport resizes (e.g., terminal resize or panel toggle).
    {
        let ctx_resize = Rc::clone(&ctx);
        main_scroll.on_resize(move |_cx, _rect| {
            let mut inner = ctx_resize.borrow_mut();
            for section in &mut inner.sections {
                apply_section_horizontal_scroll(section);
            }
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
        scroll_horizontal_by(ctx, -4.0);
    } else if key.matches("l") || key.matches("right") {
        scroll_horizontal_by(ctx, 4.0);
    } else if key.matches("pagedown") || key.matches("space") || key.matches("ctrl+d") {
        main.scroll_by(0.0, page);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("pageup") {
        main.scroll_by(0.0, -page);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("g") || key.matches("home") {
        main.scroll_to(0.0, 0.0);
        reset_horizontal_scroll(ctx);
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
    let mut sections = Vec::with_capacity(unreviewed.len());

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
                section.view.on_resize(move |_cx, rect| {
                    if !applied.replace(true) {
                        scroll.scroll_to(0.0, f64::from(rect.top).max(0.0));
                    }
                });
            }
            main_scroll.add(&section.view);
            sections.push(section);
        }
    }

    ctx.borrow_mut().sections = sections;
}

fn build_file_section(app: &App, file: &DiffFile) -> FileSection {
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

    let header_text = Text::new(app, header_spans).wrap(WrapMode::None);
    let header_width = header_text.info().width_max;

    let header = View::new(app)
        .border_sides(Sides::BOTTOM)
        .border_color(Color::BRIGHT_BLACK)
        .padding_x(1)
        .child(&header_text);

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

    let body_text = Text::new(app, diff_spans).wrap(WrapMode::None);
    let body_width = body_text.info().width_max;

    let body = View::new(app)
        .padding_x(1)
        .child(&body_text);

    let view = View::new(app)
        .column()
        .border(BorderStyle::Rounded)
        .border_color(Color::BRIGHT_BLACK)
        .margin_x(1)
        .margin_edge(Edge::Bottom, 1)
        .child(&header)
        .child(&body);

    FileSection {
        path: file.path.clone(),
        view,
        header_text,
        header_width,
        body_text,
        body_width,
        scroll_x: 0.0,
    }
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Added | FileStatus::Untracked => Color::GREEN,
        FileStatus::Deleted => Color::RED,
        FileStatus::Modified => Color::YELLOW,
        FileStatus::Renamed => Color::CYAN,
    }
}

fn text_max_scroll_x(text: &Text, content_width: u32) -> f64 {
    let visible = f64::from(text.rect().width);
    (f64::from(content_width) - visible).max(0.0).floor()
}

fn section_max_scroll_x(section: &FileSection) -> f64 {
    text_max_scroll_x(&section.header_text, section.header_width)
        .max(text_max_scroll_x(&section.body_text, section.body_width))
}

fn apply_section_horizontal_scroll(section: &mut FileSection) {
    let max_x = section_max_scroll_x(section);
    section.scroll_x = section.scroll_x.clamp(0.0, max_x).round();
    let x = section.scroll_x;
    let header_x = x.min(text_max_scroll_x(&section.header_text, section.header_width));
    let body_x = x.min(text_max_scroll_x(&section.body_text, section.body_width));
    section.header_text.scroll_to(header_x, 0.0);
    section.body_text.scroll_to(body_x, 0.0);
}

fn active_section_index(inner: &UiContext) -> Option<usize> {
    if let Some(active) = inner.state.active_file.as_deref()
        && let Some(idx) = inner.sections.iter().position(|s| s.path == active)
    {
        return Some(idx);
    }
    (!inner.sections.is_empty()).then_some(0)
}

fn scroll_horizontal_by(ctx: &Rc<RefCell<UiContext>>, dx: f64) {
    let mut inner = ctx.borrow_mut();
    if let Some(idx) = active_section_index(&inner) {
        let section = &mut inner.sections[idx];
        section.scroll_x += dx;
        apply_section_horizontal_scroll(section);
    }
}

fn scroll_horizontal_at(ctx: &Rc<RefCell<UiContext>>, x: u16, y: u16, dx: f64) {
    let mut inner = ctx.borrow_mut();
    let idx = inner
        .sections
        .iter()
        .position(|s| s.view.rect().contains(f64::from(x), f64::from(y)))
        .or_else(|| active_section_index(&inner));
    if let Some(idx) = idx {
        let section = &mut inner.sections[idx];
        section.scroll_x += dx;
        apply_section_horizontal_scroll(section);
    }
}

fn reset_horizontal_scroll(ctx: &Rc<RefCell<UiContext>>) {
    let mut inner = ctx.borrow_mut();
    if let Some(idx) = active_section_index(&inner) {
        let section = &mut inner.sections[idx];
        section.scroll_x = 0.0;
        apply_section_horizontal_scroll(section);
    }
}

fn scroll_to_file(ctx: &Rc<RefCell<UiContext>>, path: &str) {
    let mut inner = ctx.borrow_mut();
    if let Some(idx) = inner.sections.iter().position(|s| s.path == path) {
        let target_y = f64::from(inner.sections[idx].view.rect().top).max(0.0);
        inner.main_scroll.scroll_to(0.0, target_y);
        let section = &mut inner.sections[idx];
        section.scroll_x = 0.0;
        apply_section_horizontal_scroll(section);
    }
}

fn sync_active_file_from_scroll(ctx: &Rc<RefCell<UiContext>>) {
    let detected = {
        let inner = ctx.borrow();
        let viewport_top = inner.main_scroll.rect().screen_y;
        let mut active: Option<String> = inner.sections.first().map(|s| s.path.clone());
        for section in &inner.sections {
            let rel_y = section.view.rect().screen_y - viewport_top;
            if rel_y <= 1.0 {
                active = Some(section.path.clone());
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

    #[test]
    fn horizontal_scroll_keys_and_wheel_work_for_long_lines() {
        let app = TestApp::with_options(
            100,
            20,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let long_line = format!("+START_MARKER_{}_END_MARKER", "x".repeat(140));
        let files = vec![DiffFile {
            path: "src/long.rs".to_owned(),
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
                    text: long_line,
                },
            ],
        }];
        let state = AppState::new(files.clone(), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |_opts| Ok(files.clone()));
        mount(&app, state, loader);

        let initial = capture_text(&app.render());
        assert!(initial.contains("START_MARKER"), "initial screen:\n{initial}");
        assert!(!initial.contains("END_MARKER"), "initial screen should clip long line:\n{initial}");
        assert!(
            initial.contains("╭") && initial.contains("╮"),
            "initial screen should show full rounded border:\n{initial}"
        );

        // Press 'l' enough times to scroll to the end of the line.
        for _ in 0..40 {
            app.press("l");
        }
        let scrolled_right = capture_text(&app.render());
        assert!(
            scrolled_right.contains("END_MARKER"),
            "pressing 'l' should scroll right to END_MARKER:\n{scrolled_right}"
        );
        assert!(
            !scrolled_right.contains("START_MARKER"),
            "START_MARKER should be scrolled off left:\n{scrolled_right}"
        );
        // The file border and short header should remain fixed at the viewport width.
        assert!(
            scrolled_right.contains("╭") && scrolled_right.contains("╮"),
            "border should stay fixed inside viewport when scrolled right:\n{scrolled_right}"
        );
        assert!(
            scrolled_right.contains("│ M src/long.rs  +1"),
            "short header should stay fixed inside border:\n{scrolled_right}"
        );
        assert!(
            scrolled_right.contains("END_MARKER │"),
            "scrolled text should stay inside the right border:\n{scrolled_right}"
        );

        // Press 'h' enough times to scroll back to the start.
        for _ in 0..40 {
            app.press("h");
        }
        let scrolled_left = capture_text(&app.render());
        assert!(
            scrolled_left.contains("START_MARKER"),
            "pressing 'h' should scroll left back to START_MARKER:\n{scrolled_left}"
        );

        // Mouse horizontal scroll (ScrollRight) over the main panel should also work.
        for _ in 0..120 {
            app.mouse(MouseEventKind::ScrollRight, 60, 5);
        }
        let wheel_right = capture_text(&app.render());
        assert!(
            wheel_right.contains("END_MARKER"),
            "mouse ScrollRight should scroll right to END_MARKER:\n{wheel_right}"
        );
        assert!(
            wheel_right.contains("END_MARKER │"),
            "right border and padding should remain intact:\n{wheel_right}"
        );

        // Mouse ScrollLeft should scroll back to the start.
        for _ in 0..120 {
            app.mouse(MouseEventKind::ScrollLeft, 60, 5);
        }
        let wheel_left = capture_text(&app.render());
        assert!(
            wheel_left.contains("START_MARKER"),
            "mouse ScrollLeft should scroll back to START_MARKER:\n{wheel_left}"
        );

        // Shift + vertical wheel scroll should also scroll horizontally.
        for _ in 0..120 {
            app.event(opentui::input::Event::Mouse(opentui::input::MouseEvent {
                kind: MouseEventKind::ScrollDown,
                x: 60,
                y: 5,
                modifiers: Modifiers::SHIFT,
            }));
        }
        let shift_wheel = capture_text(&app.render());
        assert!(
            shift_wheel.contains("END_MARKER"),
            "Shift+ScrollDown should scroll right to END_MARKER:\n{shift_wheel}"
        );

        // Arrow keys (left / right) should also scroll horizontally.
        for _ in 0..40 {
            app.press("left");
        }
        let arrow_left = capture_text(&app.render());
        assert!(
            arrow_left.contains("START_MARKER"),
            "left arrow should scroll back to START_MARKER:\n{arrow_left}"
        );
        for _ in 0..40 {
            app.press("right");
        }
        let arrow_right = capture_text(&app.render());
        assert!(
            arrow_right.contains("END_MARKER"),
            "right arrow should scroll right to END_MARKER:\n{arrow_right}"
        );
    }

    #[test]
    fn horizontal_scroll_only_affects_current_or_hovered_file() {
        let app = TestApp::with_options(
            100,
            24,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let alpha_long = format!("+ALPHA_START_{}_ALPHA_END", "a".repeat(120));
        let beta_long = format!("+BETA_START_{}_BETA_END", "b".repeat(120));
        let files = vec![
            DiffFile {
                path: "src/alpha.rs".to_owned(),
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
                        text: alpha_long,
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
                        text: beta_long,
                    },
                ],
            },
        ];
        let state = AppState::new(files.clone(), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |_opts| Ok(files.clone()));
        mount(&app, state, loader);

        let initial = capture_text(&app.render());
        assert!(initial.contains("ALPHA_START"));
        assert!(initial.contains("BETA_START"));

        // Active file is alpha.rs: pressing 'l' should scroll only alpha.rs.
        for _ in 0..40 {
            app.press("l");
        }
        let after_l = capture_text(&app.render());
        assert!(after_l.contains("ALPHA_END"), "alpha should scroll right:\n{after_l}");
        assert!(!after_l.contains("ALPHA_START"), "alpha start should be scrolled off:\n{after_l}");
        assert!(after_l.contains("BETA_START"), "beta should stay at column 0:\n{after_l}");
        assert!(!after_l.contains("BETA_END"), "beta should not have scrolled:\n{after_l}");

        // Mouse horizontal scroll over beta.rs should scroll only beta.rs.
        let capture = app.render();
        let beta_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("BETA_START"))
            .expect("beta line should be visible") as u16;
        for _ in 0..100 {
            app.mouse(MouseEventKind::ScrollRight, 60, beta_row);
        }
        let after_mouse_beta = capture_text(&app.render());
        assert!(
            after_mouse_beta.contains("BETA_END"),
            "scrolling over beta should scroll beta right:\n{after_mouse_beta}"
        );
        assert!(
            after_mouse_beta.contains("ALPHA_END"),
            "alpha should stay where it was:\n{after_mouse_beta}"
        );
    }

    #[test]
    fn axis_lock_suppresses_accidental_cross_axis_wheel_ticks() {
        let app = TestApp::with_options(
            100,
            12,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let long_line = format!("+START_MARKER_{}_END_MARKER", "x".repeat(140));
        let mut lines = vec![
            DiffLine {
                kind: DiffLineKind::HunkHeader,
                text: "@@ -1,1 +1,20 @@".to_owned(),
            },
            DiffLine {
                kind: DiffLineKind::Added,
                text: long_line,
            },
        ];
        for i in 0..20 {
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: format!("+extra line {i}"),
            });
        }
        let files = vec![DiffFile {
            path: "src/long.rs".to_owned(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 21,
            deletions: 0,
            is_binary: false,
            lines,
        }];
        let state = AppState::new(files.clone(), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |_opts| Ok(files.clone()));
        mount(&app, state, loader);
        let _ = app.render();

        // A couple of stray horizontal ticks should be ignored by the deadzone.
        app.mouse(MouseEventKind::ScrollRight, 60, 4);
        app.mouse(MouseEventKind::ScrollRight, 60, 4);
        let after_stray = capture_text(&app.render());
        assert!(
            after_stray.contains("│ +START_MARKER_"),
            "stray ticks below deadzone should not scroll horizontally:\n{after_stray}"
        );

        // Horizontal ticks immediately following vertical scroll should be suppressed.
        app.mouse(MouseEventKind::ScrollDown, 60, 4);
        for _ in 0..10 {
            app.mouse(MouseEventKind::ScrollRight, 60, 4);
        }
        let after_vertical_lock = capture_text(&app.render());
        assert!(
            after_vertical_lock.contains("│ +START_MARKER_"),
            "horizontal ticks during vertical scroll lock should be ignored:\n{after_vertical_lock}"
        );

        // Scroll back to top, let lock expire, then perform an intentional horizontal swipe.
        app.press("g");
        let _ = app.render();
        app.advance(Duration::from_millis(150));
        for _ in 0..10 {
            app.mouse(MouseEventKind::ScrollRight, 60, 4);
        }
        let after_swipe = capture_text(&app.render());
        assert!(
            !after_swipe.contains("│ +START_MARKER_"),
            "intentional horizontal swipe after cooldown should scroll:\n{after_swipe}"
        );
        assert!(
            after_swipe.contains("M src/long.rs"),
            "header should still be visible at top before vertical scroll:\n{after_swipe}"
        );

        // Vertical ticks immediately following active horizontal scroll should be suppressed.
        for _ in 0..10 {
            app.mouse(MouseEventKind::ScrollDown, 60, 4);
        }
        let after_horizontal_lock = capture_text(&app.render());
        assert!(
            after_horizontal_lock.contains("M src/long.rs"),
            "vertical ticks during horizontal scroll lock should be ignored:\n{after_horizontal_lock}"
        );

        // After the horizontal-scroll lock expires, vertical scrolling should work again.
        app.advance(Duration::from_millis(150));
        for _ in 0..10 {
            app.mouse(MouseEventKind::ScrollDown, 60, 4);
        }
        let after_vertical_resume = capture_text(&app.render());
        assert!(
            !after_vertical_resume.contains("M src/long.rs"),
            "vertical scroll should resume after axis lock expires:\n{after_vertical_resume}"
        );
    }
}
