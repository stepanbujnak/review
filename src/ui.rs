use crate::git::{DiffFile, DiffLineKind, DiffOptions, FileStatus, LoadedDiff};
use crate::state::{ActiveItem, AppState};
use crate::tree::{TreeRowKind, build_staged_tree_rows, build_tree_rows};
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

pub type DiffLoader = Rc<dyn Fn(&DiffOptions) -> std::result::Result<LoadedDiff, String>>;
pub type FileOpener = Rc<dyn Fn(&str) -> std::result::Result<(), String>>;
pub type FileStager = Rc<dyn Fn(&str, Option<&str>) -> std::result::Result<(), String>>;
pub type FileUnstager = Rc<dyn Fn(&str, Option<&str>) -> std::result::Result<(), String>>;

struct SideFileRow {
    widget: Text,
    prefix: String,
    label: String,
    status: FileStatus,
    worktree_status: Option<FileStatus>,
    is_staged: bool,
}

struct FileSection {
    item: ActiveItem,
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
    opener: FileOpener,
    stager: FileStager,
    unstager: FileUnstager,
    side_panel: View,
    side_scroll: ScrollBox,
    main_scroll: ScrollBox,
    btn_files: Text,
    btn_config: Text,
    btn_refresh: Text,
    btn_stage: Text,
    btn_edit: Text,
    footer_status: Text,
    config_overlay: View,
    config_rows: [Text; 3],
    config_open: bool,
    config_selected: usize,
    /// Ordered list of file sections currently mounted in `main_scroll`.
    sections: Vec<FileSection>,
    /// Whether `main_scroll` is currently displaying staged diffs (`true`) or changed diffs (`false`).
    main_showing_staged: bool,
    /// Filters accidental diagonal cross-axis trackpad ticks while scrolling.
    wheel_filter: WheelAxisFilter,
    /// Map of sidebar item -> its side panel row for fast highlight updates.
    side_file_widgets: HashMap<ActiveItem, SideFileRow>,
    /// Last error message from git or editor operations, if any.
    last_error: Option<String>,
    /// Item to scroll to once the next rebuild's layout has been computed.
    pending_scroll_to: Option<ActiveItem>,
}

/// Mounts the complete reviewer UI onto `app.root()` and binds all keyboard/mouse handlers.
pub fn mount(
    app: &App,
    initial_state: AppState,
    loader: DiffLoader,
    opener: FileOpener,
    stager: FileStager,
    unstager: FileUnstager,
) {
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

    let btn_files = make_footer_button(app);
    let btn_config = make_footer_button(app);
    let btn_refresh = make_footer_button(app);
    let btn_stage = make_footer_button(app);
    let btn_edit = make_footer_button(app);

    let footer_buttons = View::new(app)
        .row()
        .gap(1)
        .shrink(0.0)
        .child(&btn_stage)
        .child(&btn_edit)
        .child(&btn_files)
        .child(&btn_config)
        .child(&btn_refresh);

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

    let cfg_wrap = make_config_row_widget(app);
    let cfg_ws = make_config_row_widget(app);
    let cfg_untracked = make_config_row_widget(app);
    let cfg_hint = Text::new(app, " ↑/↓ move · space toggle · esc ")
        .fg(Color::BRIGHT_BLACK)
        .selectable(false);

    let config_options = View::new(app)
        .column()
        .child(&cfg_wrap)
        .child(&cfg_ws)
        .child(&cfg_untracked);

    let config_dialog = View::new(app)
        .column()
        .width(40)
        .border(BorderStyle::Rounded)
        .border_color(Color::CYAN)
        .title(" Config ")
        .title_color(Color::DEFAULT_FOREGROUND)
        .bg(Color::DEFAULT_BACKGROUND)
        .padding_x(2)
        .padding_y(1)
        .gap(1)
        .child(&config_options)
        .child(&cfg_hint);

    let config_overlay = View::new(app)
        .absolute()
        .position(Edge::All, 0)
        .z_index(10)
        .justify(Justify::Center)
        .align_items(Align::Center)
        .visible(false)
        .child(&config_dialog);

    root.add(&body);
    root.add(&footer);
    root.add(&config_overlay);

    let ctx = Rc::new(RefCell::new(UiContext {
        app: app.clone(),
        state: initial_state,
        loader,
        opener,
        stager,
        unstager,
        side_panel,
        side_scroll,
        main_scroll: main_scroll.clone(),
        btn_files: btn_files.clone(),
        btn_config: btn_config.clone(),
        btn_refresh: btn_refresh.clone(),
        btn_stage: btn_stage.clone(),
        btn_edit: btn_edit.clone(),
        footer_status,
        config_overlay: config_overlay.clone(),
        config_rows: [cfg_wrap.clone(), cfg_ws.clone(), cfg_untracked.clone()],
        config_open: false,
        config_selected: 0,
        sections: Vec::new(),
        main_showing_staged: false,
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
        btn_files.on_click(move |_cx, _ev| toggle_side_panel(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_config.on_click(move |_cx, _ev| toggle_config_modal(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_refresh.on_click(move |_cx, _ev| reload_from_git(&ctx_btn));
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_stage.on_click(move |_cx, _ev| {
            if ctx_btn.borrow().state.viewing_staged() {
                unstage_current_file(&ctx_btn);
            } else {
                stage_current_file(&ctx_btn);
            }
        });
    }
    {
        let ctx_btn = Rc::clone(&ctx);
        btn_edit.on_click(move |_cx, _ev| edit_current_file(&ctx_btn));
    }

    // Wire config modal options and backdrop click-to-close.
    {
        let ctx_row = Rc::clone(&ctx);
        cfg_wrap.on_click(move |_cx, _ev| {
            ctx_row.borrow_mut().config_selected = 0;
            toggle_wrap(&ctx_row);
        });
    }
    {
        let ctx_row = Rc::clone(&ctx);
        cfg_ws.on_click(move |_cx, _ev| {
            ctx_row.borrow_mut().config_selected = 1;
            toggle_whitespace(&ctx_row);
        });
    }
    {
        let ctx_row = Rc::clone(&ctx);
        cfg_untracked.on_click(move |_cx, _ev| {
            ctx_row.borrow_mut().config_selected = 2;
            toggle_untracked(&ctx_row);
        });
    }
    {
        let ctx_bg = Rc::clone(&ctx);
        let dialog_ref = config_dialog.clone();
        config_overlay.on_click(move |_cx, ev| {
            if !dialog_ref.rect().contains(f64::from(ev.x), f64::from(ev.y)) {
                set_config_open(&ctx_bg, false);
            }
        });
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
            let wrap_lines = inner.state.wrap_lines;
            for section in &mut inner.sections {
                apply_section_horizontal_scroll(section, wrap_lines);
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

fn make_config_row_widget(app: &App) -> Text {
    Text::new(app, "")
        .selectable(false)
        .mouse_pointer(MousePointer::POINTER)
}

fn set_config_open(ctx: &Rc<RefCell<UiContext>>, open: bool) {
    let mut inner = ctx.borrow_mut();
    if inner.config_open == open {
        return;
    }
    inner.config_open = open;
    inner.config_overlay.visible(open);
    update_footer(&inner);
    update_config_modal(&inner);
}

fn toggle_config_modal(ctx: &Rc<RefCell<UiContext>>) {
    let open = !ctx.borrow().config_open;
    set_config_open(ctx, open);
}

fn toggle_selected_config(ctx: &Rc<RefCell<UiContext>>) {
    let selected = ctx.borrow().config_selected;
    match selected {
        0 => toggle_wrap(ctx),
        1 => toggle_whitespace(ctx),
        _ => toggle_untracked(ctx),
    }
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

fn toggle_wrap(ctx: &Rc<RefCell<UiContext>>) {
    let mut inner = ctx.borrow_mut();
    inner.state.wrap_lines = !inner.state.wrap_lines;
    let wrap_lines = inner.state.wrap_lines;
    let mode = wrap_mode(wrap_lines);
    for section in &mut inner.sections {
        section.header_text.wrap(mode);
        section.body_text.wrap(mode);
        apply_section_horizontal_scroll(section, wrap_lines);
    }
    update_footer(&inner);
}

fn wrap_mode(wrap_lines: bool) -> WrapMode {
    if wrap_lines {
        WrapMode::Char
    } else {
        WrapMode::None
    }
}

fn stage_current_file(ctx: &Rc<RefCell<UiContext>>) {
    let (stager, path, old_path, next_target) = {
        let inner = ctx.borrow();
        if !inner.state.can_stage() {
            return;
        }
        let Some(file) = inner.state.active_changed_file() else {
            return;
        };
        let next_target = inner.state.next_item_after_staging(&file.path);
        (
            Rc::clone(&inner.stager),
            file.path.clone(),
            file.old_path.clone(),
            next_target,
        )
    };

    match stager(&path, old_path.as_deref()) {
        Ok(()) => reload_from_git_with_preferred(ctx, next_target),
        Err(err) => {
            let mut inner = ctx.borrow_mut();
            inner.last_error = Some(err);
            update_footer(&inner);
        }
    }
}

fn unstage_current_file(ctx: &Rc<RefCell<UiContext>>) {
    let (unstager, path, old_path, next_target) = {
        let inner = ctx.borrow();
        if !inner.state.can_stage() {
            return;
        }
        let Some(file) = inner.state.active_staged_file() else {
            return;
        };
        let next_target = inner.state.next_item_after_unstaging(&file.path);
        (
            Rc::clone(&inner.unstager),
            file.path.clone(),
            file.old_path.clone(),
            next_target,
        )
    };

    match unstager(&path, old_path.as_deref()) {
        Ok(()) => reload_from_git_with_preferred(ctx, next_target),
        Err(err) => {
            let mut inner = ctx.borrow_mut();
            inner.last_error = Some(err);
            update_footer(&inner);
        }
    }
}

fn select_item(ctx: &Rc<RefCell<UiContext>>, target: ActiveItem) {
    let needs_main_rebuild = {
        let inner = ctx.borrow();
        target.is_staged() != inner.main_showing_staged
    };
    update_active_highlight(ctx, Some(target.clone()));
    if needs_main_rebuild {
        ctx.borrow_mut().pending_scroll_to = Some(target);
        rebuild_main_panel(ctx);
    } else {
        scroll_to_item(ctx, &target);
    }
}

fn edit_current_file(ctx: &Rc<RefCell<UiContext>>) {
    let (app, opener, path) = {
        let inner = ctx.borrow();
        let Some(path) = inner.state.active_path().map(str::to_owned) else {
            return;
        };
        (inner.app.clone(), Rc::clone(&inner.opener), path)
    };

    let ctx_after = Rc::clone(ctx);
    app.release_terminal(move |_cx| match opener(&path) {
        Ok(()) => reload_from_git(&ctx_after),
        Err(err) => {
            let mut inner = ctx_after.borrow_mut();
            inner.last_error = Some(err);
            update_footer(&inner);
        }
    });
}

fn handle_key(cx: &mut Cx<'_>, key: &KeyEvent, ctx: &Rc<RefCell<UiContext>>) {
    if key.matches("ctrl+c") {
        cx.quit();
        return;
    }

    if ctx.borrow().config_open {
        handle_config_modal_key(key, ctx);
        return;
    }

    if key.matches("q") {
        cx.quit();
        return;
    }

    if key.matches("f") {
        toggle_side_panel(ctx);
        return;
    }

    if key.matches("c") {
        toggle_config_modal(ctx);
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

    if key.matches("ctrl+z") {
        toggle_wrap(ctx);
        return;
    }

    if key.matches("s") {
        stage_current_file(ctx);
        return;
    }

    if key.matches("u") {
        unstage_current_file(ctx);
        return;
    }

    if key.matches("e") {
        edit_current_file(ctx);
        return;
    }

    if key.matches("r") || key.matches("R") {
        reload_from_git(ctx);
        return;
    }

    if key.matches("n") {
        let target = ctx.borrow().state.next_active_item(1);
        if let Some(item) = target {
            select_item(ctx, item);
        }
        return;
    }

    if key.matches("p") {
        let target = ctx.borrow().state.next_active_item(-1);
        if let Some(item) = target {
            select_item(ctx, item);
        }
        return;
    }

    // Vim & standard scrolling in the main diff view.
    let main = ctx.borrow().main_scroll.clone();
    let page = (f64::from(main.rect().height) / 2.0).max(1.0).floor();

    if key.matches("j") || key.matches("down") {
        scroll_vertical_by(&main, 1.0);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("k") || key.matches("up") {
        scroll_vertical_by(&main, -1.0);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("h") || key.matches("left") {
        scroll_horizontal_by(ctx, -4.0);
    } else if key.matches("l") || key.matches("right") {
        scroll_horizontal_by(ctx, 4.0);
    } else if key.matches("pagedown") || key.matches("space") || key.matches("ctrl+d") {
        scroll_vertical_by(&main, page);
        sync_active_file_from_scroll(ctx);
    } else if key.matches("pageup") {
        scroll_vertical_by(&main, -page);
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

/// Scrolls vertically from the current clamped position without letting upward scrolls
/// accumulate negative pending offsets at the top edge.
fn scroll_vertical_by(scroll: &ScrollBox, dy: f64) {
    let (x, y) = scroll.scroll_position();
    scroll.scroll_to(x, (y + dy).max(0.0));
}

fn handle_config_modal_key(key: &KeyEvent, ctx: &Rc<RefCell<UiContext>>) {
    if key.matches("escape") || key.matches("c") || key.matches("q") {
        set_config_open(ctx, false);
        return;
    }

    if key.matches("j") || key.matches("down") {
        let mut inner = ctx.borrow_mut();
        inner.config_selected = (inner.config_selected + 1) % inner.config_rows.len();
        update_config_modal(&inner);
        return;
    }

    if key.matches("k") || key.matches("up") {
        let mut inner = ctx.borrow_mut();
        let len = inner.config_rows.len();
        inner.config_selected = (inner.config_selected + len - 1) % len;
        update_config_modal(&inner);
        return;
    }

    if key.matches("enter") || key.matches("space") {
        toggle_selected_config(ctx);
        return;
    }

    if key.matches("z") || key.matches("ctrl+z") {
        ctx.borrow_mut().config_selected = 0;
        toggle_wrap(ctx);
        return;
    }

    if key.matches("w") || key.matches("ctrl+w") {
        ctx.borrow_mut().config_selected = 1;
        toggle_whitespace(ctx);
        return;
    }

    if key.matches("u") || key.matches("ctrl+u") {
        ctx.borrow_mut().config_selected = 2;
        toggle_untracked(ctx);
    }
}

fn reload_from_git(ctx: &Rc<RefCell<UiContext>>) {
    reload_from_git_with_preferred(ctx, None);
}

fn reload_from_git_with_preferred(
    ctx: &Rc<RefCell<UiContext>>,
    preferred_active: Option<ActiveItem>,
) {
    let (loader, opts) = {
        let inner = ctx.borrow();
        (Rc::clone(&inner.loader), inner.state.diff_options())
    };
    match loader(&opts) {
        Ok(loaded) => {
            let target_item = {
                let mut inner = ctx.borrow_mut();
                inner.last_error = None;
                inner.state.reload_files(loaded, preferred_active);
                let active = inner.state.active_item.clone();
                inner.pending_scroll_to = active.clone();
                active
            };
            rebuild_all(ctx);
            if target_item.is_none() {
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
    let (
        app,
        side_scroll,
        can_stage,
        staged_rows,
        changed_rows,
        active_item,
        staged_count,
        changed_count,
    ) = {
        let inner = ctx.borrow();
        let staged = inner.state.staged_files();
        let changed = inner.state.changed_files();
        let s_rows = build_staged_tree_rows(&staged, &inner.state.collapsed_staged_dirs);
        let c_rows = build_tree_rows(&changed, &inner.state.collapsed_dirs);
        (
            inner.app.clone(),
            inner.side_scroll.clone(),
            inner.state.can_stage(),
            s_rows,
            c_rows,
            inner.state.active_item.clone(),
            staged.len(),
            changed.len(),
        )
    };

    side_scroll.clear();
    let mut file_widgets = HashMap::new();

    // Staged section at the top of the sidebar (shown only when there are staged files).
    if can_stage && staged_count > 0 {
        let staged_header = Text::new(&app, format!("Staged ({staged_count})"))
            .fg(Color::DEFAULT_FOREGROUND)
            .bold()
            .selectable(false);
        side_scroll.add(&staged_header);

        for row in staged_rows {
            match row.kind {
                TreeRowKind::Dir { path, .. } => {
                    let text = Text::new(&app, format!("{}{}", row.prefix, row.label))
                        .fg(Color::CYAN)
                        .truncate(true)
                        .selectable(false)
                        .mouse_pointer(MousePointer::POINTER);
                    let ctx_click = Rc::clone(ctx);
                    text.on_click(move |_cx, _ev| {
                        ctx_click.borrow_mut().state.toggle_staged_dir(&path);
                        rebuild_side_panel(&ctx_click);
                    });
                    side_scroll.add(&text);
                }
                TreeRowKind::File {
                    path,
                    status,
                    worktree_status,
                    ..
                } => {
                    let item = ActiveItem::Staged(path);
                    let is_active = active_item.as_ref() == Some(&item);
                    let text = Text::new(
                        &app,
                        build_staged_file_spans(
                            &row.prefix,
                            &row.label,
                            status,
                            worktree_status,
                            is_active,
                        ),
                    )
                    .truncate(true)
                    .selectable(false)
                    .mouse_pointer(MousePointer::POINTER);
                    let ctx_click = Rc::clone(ctx);
                    let click_item = item.clone();
                    text.on_click(move |_cx, _ev| {
                        select_item(&ctx_click, click_item.clone());
                    });
                    file_widgets.insert(
                        item,
                        SideFileRow {
                            widget: text.clone(),
                            prefix: row.prefix,
                            label: row.label,
                            status,
                            worktree_status,
                            is_staged: true,
                        },
                    );
                    side_scroll.add(&text);
                }
            }
        }

        let spacer = Text::new(&app, "").selectable(false);
        side_scroll.add(&spacer);
    }

    // Changed (unstaged) section.
    let changed_header = Text::new(&app, format!("Changed ({changed_count})"))
        .fg(Color::DEFAULT_FOREGROUND)
        .bold()
        .selectable(false);
    side_scroll.add(&changed_header);

    if changed_rows.is_empty() {
        let empty = Text::new(&app, "  (none)")
            .fg(Color::BRIGHT_BLACK)
            .selectable(false);
        side_scroll.add(&empty);
    } else {
        for row in changed_rows {
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
                    let item = ActiveItem::Changed(path);
                    let is_active = active_item.as_ref() == Some(&item);
                    let text = build_side_file_row(&app, &row.prefix, &row.label, status, is_active);
                    let ctx_click = Rc::clone(ctx);
                    let click_item = item.clone();
                    text.on_click(move |_cx, _ev| {
                        select_item(&ctx_click, click_item.clone());
                    });
                    file_widgets.insert(
                        item,
                        SideFileRow {
                            widget: text.clone(),
                            prefix: row.prefix,
                            label: row.label,
                            status,
                            worktree_status: None,
                            is_staged: false,
                        },
                    );
                    side_scroll.add(&text);
                }
            }
        }
    }

    ctx.borrow_mut().side_file_widgets = file_widgets;
}

fn build_staged_file_spans(
    prefix: &str,
    label: &str,
    status: FileStatus,
    worktree_status: Option<FileStatus>,
    active: bool,
) -> Vec<Span<'static>> {
    if active {
        let badge = match worktree_status {
            Some(wt) => format!("{}{}", status.badge(), wt.badge()),
            None => status.badge().to_owned(),
        };
        return vec![
            Span::styled(prefix.to_owned(), Style::new().fg(Color::BRIGHT_BLACK)),
            Span::styled(
                format!(" {badge} {label} "),
                Style::new().fg(Color::DEFAULT_FOREGROUND).bold().inverse(),
            ),
        ];
    }

    let mut spans = vec![Span::styled(
        prefix.to_owned(),
        Style::new().fg(Color::BRIGHT_BLACK),
    )];
    if let Some(wt) = worktree_status {
        spans.push(Span::styled(
            status.badge().to_owned(),
            Style::new().fg(status_color(status)),
        ));
        spans.push(Span::styled(
            format!("{} ", wt.badge()),
            Style::new().fg(status_color(wt)).bold(),
        ));
    } else {
        spans.push(Span::styled(
            format!("{} ", status.badge()),
            Style::new().fg(status_color(status)),
        ));
    }
    spans.push(Span::styled(
        label.to_owned(),
        Style::new().fg(Color::DEFAULT_FOREGROUND),
    ));
    spans
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
    let (app, main_scroll, showing_staged, files, staged_count, pending_target, wrap_lines) = {
        let mut inner = ctx.borrow_mut();
        let showing_staged = inner.state.viewing_staged();
        let files: Vec<DiffFile> = if showing_staged {
            inner.state.staged_diff_files()
        } else {
            inner
                .state
                .changed_files()
                .into_iter()
                .cloned()
                .collect()
        };
        (
            inner.app.clone(),
            inner.main_scroll.clone(),
            showing_staged,
            files,
            inner.state.staged_count(),
            inner.pending_scroll_to.take(),
            inner.state.wrap_lines,
        )
    };

    main_scroll.clear();
    let mut sections = Vec::with_capacity(files.len());

    if files.is_empty() {
        let message = if staged_count == 0 {
            "No git changes."
        } else {
            "All changes staged. Select a staged file in the files panel (f) or press p to inspect/unstage (u)."
        };
        let empty = View::new(&app)
            .padding(2)
            .child(&Text::new(&app, message).fg(Color::BRIGHT_BLACK));
        main_scroll.add(&empty);
    } else {
        for file in &files {
            let item = if showing_staged {
                ActiveItem::Staged(file.path.clone())
            } else {
                ActiveItem::Changed(file.path.clone())
            };
            let section = build_file_section(&app, file, item.clone(), wrap_lines);
            if pending_target.as_ref() == Some(&item) {
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

    let mut inner = ctx.borrow_mut();
    inner.sections = sections;
    inner.main_showing_staged = showing_staged;
}

fn build_file_section(
    app: &App,
    file: &DiffFile,
    item: ActiveItem,
    wrap_lines: bool,
) -> FileSection {
    let mode = wrap_mode(wrap_lines);
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

    let header_text = Text::new(app, header_spans).wrap(mode);
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

    let body_text = Text::new(app, diff_spans).wrap(mode);
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
        item,
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

fn apply_section_horizontal_scroll(section: &mut FileSection, wrap_lines: bool) {
    if wrap_lines {
        section.scroll_x = 0.0;
        section.header_text.scroll_to(0.0, 0.0);
        section.body_text.scroll_to(0.0, 0.0);
        return;
    }
    let max_x = section_max_scroll_x(section);
    section.scroll_x = section.scroll_x.clamp(0.0, max_x).round();
    let x = section.scroll_x;
    let header_x = x.min(text_max_scroll_x(&section.header_text, section.header_width));
    let body_x = x.min(text_max_scroll_x(&section.body_text, section.body_width));
    section.header_text.scroll_to(header_x, 0.0);
    section.body_text.scroll_to(body_x, 0.0);
}

fn active_section_index(inner: &UiContext) -> Option<usize> {
    if let Some(active) = inner.state.active_item.as_ref()
        && let Some(idx) = inner.sections.iter().position(|s| &s.item == active)
    {
        return Some(idx);
    }
    (!inner.sections.is_empty()).then_some(0)
}

fn scroll_horizontal_by(ctx: &Rc<RefCell<UiContext>>, dx: f64) {
    let mut inner = ctx.borrow_mut();
    if inner.state.wrap_lines {
        return;
    }
    if let Some(idx) = active_section_index(&inner) {
        let section = &mut inner.sections[idx];
        section.scroll_x += dx;
        apply_section_horizontal_scroll(section, false);
    }
}

fn scroll_horizontal_at(ctx: &Rc<RefCell<UiContext>>, x: u16, y: u16, dx: f64) {
    let mut inner = ctx.borrow_mut();
    if inner.state.wrap_lines {
        return;
    }
    let idx = inner
        .sections
        .iter()
        .position(|s| s.view.rect().contains(f64::from(x), f64::from(y)))
        .or_else(|| active_section_index(&inner));
    if let Some(idx) = idx {
        let section = &mut inner.sections[idx];
        section.scroll_x += dx;
        apply_section_horizontal_scroll(section, false);
    }
}

fn reset_horizontal_scroll(ctx: &Rc<RefCell<UiContext>>) {
    let mut inner = ctx.borrow_mut();
    let wrap_lines = inner.state.wrap_lines;
    if let Some(idx) = active_section_index(&inner) {
        let section = &mut inner.sections[idx];
        section.scroll_x = 0.0;
        apply_section_horizontal_scroll(section, wrap_lines);
    }
}

fn scroll_to_item(ctx: &Rc<RefCell<UiContext>>, target: &ActiveItem) {
    let mut inner = ctx.borrow_mut();
    let wrap_lines = inner.state.wrap_lines;
    if let Some(idx) = inner.sections.iter().position(|s| &s.item == target) {
        let target_y = f64::from(inner.sections[idx].view.rect().top).max(0.0);
        inner.main_scroll.scroll_to(0.0, target_y);
        let section = &mut inner.sections[idx];
        section.scroll_x = 0.0;
        apply_section_horizontal_scroll(section, wrap_lines);
    }
}

fn sync_active_file_from_scroll(ctx: &Rc<RefCell<UiContext>>) {
    let detected = {
        let inner = ctx.borrow();
        let viewport_top = inner.main_scroll.rect().screen_y;
        let mut active: Option<ActiveItem> = inner.sections.first().map(|s| s.item.clone());
        for section in &inner.sections {
            let rel_y = section.view.rect().screen_y - viewport_top;
            if rel_y <= 1.0 {
                active = Some(section.item.clone());
            } else {
                break;
            }
        }
        active
    };

    if detected != ctx.borrow().state.active_item {
        update_active_highlight(ctx, detected);
    }
}

fn render_side_row(row: &SideFileRow, active: bool) {
    if row.is_staged {
        row.widget.set_text(build_staged_file_spans(
            &row.prefix,
            &row.label,
            row.status,
            row.worktree_status,
            active,
        ));
    } else {
        row.widget
            .set_text(build_side_file_spans(&row.prefix, &row.label, row.status, active));
    }
}

fn update_active_highlight(ctx: &Rc<RefCell<UiContext>>, new_active: Option<ActiveItem>) {
    let mut inner = ctx.borrow_mut();
    let old_active = inner.state.active_item.clone();
    if old_active == new_active {
        return;
    }
    if let Some(old) = old_active.as_ref()
        && let Some(row) = inner.side_file_widgets.get(old)
    {
        render_side_row(row, false);
    }
    if let Some(new) = new_active.as_ref()
        && let Some(row) = inner.side_file_widgets.get(new)
    {
        render_side_row(row, true);
    }
    inner.state.active_item = new_active;
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

fn format_config_row(label: &str, shortcut: &str, enabled: bool, selected: bool) -> Vec<Span<'static>> {
    let check = if enabled { "[x]" } else { "[ ]" };
    if selected {
        vec![Span::styled(
            format!(" {check} {label:<20} ({shortcut}) "),
            Style::new().fg(Color::DEFAULT_FOREGROUND).bold().inverse(),
        )]
    } else {
        vec![
            Span::styled(
                format!(" {check} "),
                Style::new().fg(if enabled {
                    Color::GREEN
                } else {
                    Color::BRIGHT_BLACK
                }),
            ),
            Span::styled(
                format!("{label:<20} "),
                Style::new().fg(Color::DEFAULT_FOREGROUND),
            ),
            Span::styled(format!("({shortcut}) "), Style::new().fg(Color::BRIGHT_BLACK)),
        ]
    }
}

fn update_config_modal(inner: &UiContext) {
    let items = [
        ("Wrap lines", "^z", inner.state.wrap_lines),
        ("Whitespace changes", "^w", !inner.state.hide_whitespace),
        ("Untracked files", "^u", !inner.state.hide_untracked),
    ];
    for (idx, (label, shortcut, enabled)) in items.into_iter().enumerate() {
        inner.config_rows[idx].set_text(format_config_row(
            label,
            shortcut,
            enabled,
            idx == inner.config_selected,
        ));
    }
}

fn update_footer(inner: &UiContext) {
    let files_on = !inner.state.hide_side_panel;

    style_toggle_button(&inner.btn_files, "f files", files_on);
    style_toggle_button(&inner.btn_config, "c config", inner.config_open);
    inner
        .btn_refresh
        .set_text(" r refresh ")
        .fg(Color::WHITE)
        .bg(Color::BRIGHT_BLACK)
        .attributes(Attributes::NONE);

    if inner.state.can_stage() && inner.state.viewing_staged() {
        inner
            .btn_stage
            .set_text(" u unstage ")
            .fg(Color::BLACK)
            .bg(Color::YELLOW)
            .attributes(Attributes::BOLD);
    } else if inner.state.can_stage() && inner.state.active_changed_file().is_some() {
        inner
            .btn_stage
            .set_text(" s stage ")
            .fg(Color::BLACK)
            .bg(Color::GREEN)
            .attributes(Attributes::BOLD);
    } else {
        inner
            .btn_stage
            .set_text(" s stage ")
            .fg(Color::WHITE)
            .bg(Color::BRIGHT_BLACK)
            .attributes(Attributes::DIM);
    }

    if inner.state.active_path().is_some() {
        inner
            .btn_edit
            .set_text(" e edit ")
            .fg(Color::BLACK)
            .bg(Color::CYAN)
            .attributes(Attributes::BOLD);
    } else {
        inner
            .btn_edit
            .set_text(" e edit ")
            .fg(Color::WHITE)
            .bg(Color::BRIGHT_BLACK)
            .attributes(Attributes::DIM);
    }

    if let Some(err) = &inner.last_error {
        inner.footer_status.set_text(format!("error: {err}")).fg(Color::RED);
    } else if inner.state.can_stage() {
        let staged = inner.state.staged_count();
        let changed = inner.state.changed_count();
        let status_text = if changed == 0 {
            format!("{staged} staged")
        } else {
            format!("{staged} staged · {changed} changed")
        };
        inner
            .footer_status
            .set_text(status_text)
            .fg(Color::BRIGHT_BLACK);
    } else {
        inner
            .footer_status
            .set_text(format!("{} changed", inner.state.changed_count()))
            .fg(Color::BRIGHT_BLACK);
    }

    update_config_modal(inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, DiffTarget, StagedFile};
    use opentui::testing::TestApp;
    use std::collections::HashSet;

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

    fn noop_opener() -> FileOpener {
        Rc::new(|_path| Ok(()))
    }

    fn noop_stager() -> FileStager {
        Rc::new(|_path, _old| Ok(()))
    }

    fn noop_unstager() -> FileUnstager {
        Rc::new(|_path, _old| Ok(()))
    }

    fn setup_test_app() -> TestApp {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let staged_set: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));

        let load_fn = {
            let staged_set = Rc::clone(&staged_set);
            move |opts: &DiffOptions| {
                let staged_paths = staged_set.borrow();
                let all = sample_files(opts);
                let mut files = Vec::new();
                let mut staged = Vec::new();
                let mut staged_diffs = Vec::new();
                for f in all {
                    if staged_paths.contains(&f.path) {
                        let staged_status = if f.status == FileStatus::Untracked {
                            FileStatus::Added
                        } else {
                            f.status
                        };
                        staged.push(StagedFile {
                            path: f.path.clone(),
                            old_path: f.old_path.clone(),
                            status: staged_status,
                            worktree_status: None,
                        });
                        let mut staged_diff = f;
                        staged_diff.status = staged_status;
                        staged_diffs.push(staged_diff);
                    } else {
                        files.push(f);
                    }
                }
                Ok(LoadedDiff {
                    files,
                    staged,
                    staged_diffs,
                })
            }
        };

        let initial = load_fn(&DiffOptions::default()).unwrap();
        let state = AppState::new(initial, DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(load_fn);
        let stager: FileStager = {
            let staged_set = Rc::clone(&staged_set);
            Rc::new(move |path, _old| {
                staged_set.borrow_mut().insert(path.to_owned());
                Ok(())
            })
        };
        let unstager: FileUnstager = {
            let staged_set = Rc::clone(&staged_set);
            Rc::new(move |path, _old| {
                staged_set.borrow_mut().remove(path);
                Ok(())
            })
        };
        mount(&app, state, loader, noop_opener(), stager, unstager);
        app
    }

    #[test]
    fn toggles_side_panel_whitespace_and_untracked() {
        let app = setup_test_app();

        // Initial state: no staged section when 0 staged, Changed (3) shown, whitespace hidden, untracked shown.
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Staged ("));
        assert!(screen.contains("Changed (3)"));
        assert!(screen.contains("untracked.txt"));
        assert!(!screen.contains("ws_only.txt"));

        // 'f' hides files panel, and pressing again restores it.
        app.press("f");
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Changed (3)"));
        app.press("f");
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
    fn stage_key_moves_file_to_staged_section_at_top_with_status() {
        let app = setup_test_app();
        let before = capture_text(&app.render());
        assert!(before.contains("+new alpha"));
        assert!(!before.contains("Staged ("));

        // Stage active file (src/alpha.rs) with 's'.
        app.press("s");
        let capture = app.render();
        let after = capture_text(&capture);
        assert!(!after.contains("+new alpha"));
        assert!(after.contains("Staged (1)"));
        assert!(after.contains("Changed (2)"));
        assert!(after.contains("1 staged · 2 changed"));

        // Staged section appears above Changed section and shows status badge `M alpha.rs`.
        let staged_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("Staged (1)"))
            .expect("Staged header");
        let changed_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("Changed (2)"))
            .expect("Changed header");
        assert!(staged_row < changed_row, "Staged should appear above Changed");

        let alpha_row = (staged_row..changed_row)
            .find(|&row| capture.row(row).contains("M alpha.rs"))
            .expect("staged alpha.rs should show M status badge in Staged section");
        assert!(alpha_row > staged_row && alpha_row < changed_row);
    }

    #[test]
    fn staged_but_updated_file_shows_both_statuses_in_sidebar() {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let loaded = LoadedDiff {
            files: vec![DiffFile {
                path: "src/alpha.rs".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    text: "+updated after stage".to_owned(),
                }],
            }],
            staged: vec![
                StagedFile {
                    path: "src/alpha.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Modified,
                    worktree_status: Some(FileStatus::Modified),
                },
                StagedFile {
                    path: "new.txt".to_owned(),
                    old_path: None,
                    status: FileStatus::Added,
                    worktree_status: Some(FileStatus::Deleted),
                },
            ],
            staged_diffs: Vec::new(),
        };
        let state = AppState::new(loaded.clone(), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |_opts| Ok(loaded.clone()));
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(screen.contains("Staged (2)"), "screen:\n{screen}");
        assert!(screen.contains("MM alpha.rs"), "should show MM for staged+modified:\n{screen}");
        assert!(screen.contains("AD new.txt"), "should show AD for staged-added+deleted:\n{screen}");
        assert!(screen.contains("Changed (1)"), "screen:\n{screen}");
    }

    #[test]
    fn click_or_prev_highlights_staged_file_and_u_unstages_and_hides_empty_section() {
        let app = setup_test_app();
        app.press("s");
        let capture = app.render();

        let staged_header = (0..capture.height)
            .find(|&row| capture.row(row).contains("Staged (1)"))
            .expect("Staged header");
        let changed_header = (0..capture.height)
            .find(|&row| capture.row(row).contains("Changed (2)"))
            .expect("Changed header");
        let staged_row = (staged_header..changed_header)
            .find(|&row| capture.row(row).contains("M alpha.rs"))
            .expect("staged alpha.rs row should be visible in side panel");

        // Clicking a staged file highlights it and shows its staged diff without unstaging it.
        app.click(6, staged_row as u16);
        let selected = capture_text(&app.render());
        assert!(selected.contains("Staged (1)"), "clicking should not unstage:\n{selected}");
        assert!(selected.contains(" M alpha.rs "), "staged file should be highlighted:\n{selected}");
        assert!(selected.contains("+new alpha"), "should show staged diff:\n{selected}");
        assert!(selected.contains(" u unstage "), "footer should show u unstage:\n{selected}");

        // Pressing 'n' navigates from Staged back down into Changed (beta.rs).
        app.press("n");
        let back_to_changed = capture_text(&app.render());
        assert!(back_to_changed.contains(" M beta.rs "));
        assert!(back_to_changed.contains(" s stage "));

        // Pressing 'p' navigates from Changed up into Staged (alpha.rs).
        app.press("p");
        let back_to_staged = capture_text(&app.render());
        assert!(back_to_staged.contains(" M alpha.rs "));
        assert!(back_to_staged.contains(" u unstage "));

        // Pressing 'u' unstages alpha.rs; with 0 staged files remaining, Staged section disappears.
        app.press("u");
        let restored = capture_text(&app.render());
        assert!(restored.contains("+new alpha"));
        assert!(!restored.contains("Staged ("), "Staged section should disappear when empty:\n{restored}");
        assert!(restored.contains("Changed (3)"));
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

        let state = AppState::new(
            LoadedDiff {
                files: make_tall_files(&DiffOptions::default()),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        let loader: DiffLoader = Rc::new(move |opts| {
            Ok(LoadedDiff {
                files: make_tall_files(opts),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());
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
    fn clicking_footer_buttons_and_config_modal_works() {
        let app = setup_test_app();
        let capture = app.render();
        let footer_y = (capture.height - 1) as u16;
        let footer_row = capture.row(capture.height - 1);

        // Footer shows compact buttons: s stage, e edit, f files, c config, r refresh.
        let stage_pos = footer_row.find(" s stage ").expect("stage button");
        let edit_pos = footer_row.find(" e edit ").expect("edit button");
        let files_pos = footer_row.find(" f files ").expect("files button");
        let config_pos = footer_row.find(" c config ").expect("config button");
        let refresh_pos = footer_row.find(" r refresh ").expect("refresh button");
        assert!(
            stage_pos < edit_pos
                && edit_pos < files_pos
                && files_pos < config_pos
                && config_pos < refresh_pos
        );
        assert!(!footer_row.contains("^w ws"));
        assert!(!footer_row.contains("^u untracked"));
        assert!(!footer_row.contains("^z wrap"));

        // Click files button to hide, then click again to restore.
        let files_x = files_pos as u16 + 2;
        app.click(files_x, footer_y);
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Changed (3)"));
        app.click(files_x, footer_y);
        let screen = capture_text(&app.render());
        assert!(screen.contains("Changed (3)"));

        // Click config button to open the modal.
        let config_x = config_pos as u16 + 2;
        app.click(config_x, footer_y);
        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(screen.contains("Config"), "config modal should open:\n{screen}");
        assert!(screen.contains("[ ] Wrap lines"));
        assert!(screen.contains("[ ] Whitespace changes"));
        assert!(screen.contains("[x] Untracked files"));

        // Click Whitespace changes inside the modal.
        let ws_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("Whitespace changes"))
            .expect("whitespace row in config modal");
        let ws_x = capture.row(ws_row).find("Whitespace changes").unwrap() as u16 + 2;
        app.click(ws_x, ws_row as u16);
        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(screen.contains("[x] Whitespace changes"));
        assert!(screen.contains("ws_only.txt"));

        // Click Untracked files inside the modal.
        let untracked_row = (0..capture.height)
            .find(|&row| capture.row(row).contains("Untracked files"))
            .expect("untracked row in config modal");
        let untracked_x = capture.row(untracked_row).find("Untracked files").unwrap() as u16 + 2;
        app.click(untracked_x, untracked_row as u16);
        let capture = app.render();
        let screen = capture_text(&capture);
        assert!(screen.contains("[ ] Untracked files"));
        assert!(!screen.contains("untracked.txt"));

        // Click outside the modal to close it.
        app.click(1, 1);
        let screen = capture_text(&app.render());
        assert!(!screen.contains("Whitespace changes"));

        // Click stage button.
        let stage_x = stage_pos as u16 + 2;
        app.click(stage_x, footer_y);
        let screen = capture_text(&app.render());
        assert!(screen.contains("Staged (1)"));
        assert!(screen.contains("1 staged · 2 changed"));
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
        let state = AppState::new(
            LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        let loader: DiffLoader = Rc::new(move |_opts| {
            Ok(LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

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
        let state = AppState::new(
            LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        let loader: DiffLoader = Rc::new(move |_opts| {
            Ok(LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

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
        let state = AppState::new(
            LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        let loader: DiffLoader = Rc::new(move |_opts| {
            Ok(LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());
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

    #[test]
    fn toggle_wrap_via_shortcut_and_footer_button_wraps_long_lines() {
        let app = TestApp::with_options(
            100,
            20,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let long_line = format!("+START_MARKER_{}_END_MARKER", "x".repeat(80));
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
        let state = AppState::new(
            LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        let loader: DiffLoader = Rc::new(move |_opts| {
            Ok(LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

        // Wrap is off by default: long line is clipped.
        let initial = capture_text(&app.render());
        assert!(initial.contains("START_MARKER"));
        assert!(!initial.contains("END_MARKER"));

        // Scroll horizontally first, then toggle wrap on with ctrl+z:
        // horizontal offset should reset and both START_MARKER and END_MARKER should be visible.
        for _ in 0..10 {
            app.press("l");
        }
        app.press("ctrl+z");
        let wrapped = capture_text(&app.render());
        assert!(
            wrapped.contains("START_MARKER") && wrapped.contains("END_MARKER"),
            "wrapped view should show both start and end of long line:\n{wrapped}"
        );

        // Open config modal via 'c', toggle Wrap lines off with space, and close with escape.
        app.press("c");
        let modal = capture_text(&app.render());
        assert!(modal.contains("[x] Wrap lines"), "modal should reflect wrap=on:\n{modal}");
        app.press("space");
        let modal_after = capture_text(&app.render());
        assert!(modal_after.contains("[ ] Wrap lines"));
        app.press("escape");

        let unwrapped = capture_text(&app.render());
        assert!(!unwrapped.contains("Wrap lines"));
        assert!(unwrapped.contains("START_MARKER"));
        assert!(!unwrapped.contains("END_MARKER"));
    }

    #[test]
    fn config_modal_keyboard_navigation_and_isolation() {
        let app = setup_test_app();
        let _ = app.render();

        // Open config modal with 'c'.
        app.press("c");
        let modal = capture_text(&app.render());
        assert!(modal.contains("[ ] Wrap lines"));
        assert!(modal.contains("[ ] Whitespace changes"));
        assert!(modal.contains("[x] Untracked files"));

        // Pressing 's' while modal is open should NOT stage a file.
        app.press("s");
        let after_s = capture_text(&app.render());
        assert!(after_s.contains("0 staged · 3 changed"));

        // Move down to Whitespace changes and toggle with enter.
        app.press("j");
        app.press("enter");
        let after_ws = capture_text(&app.render());
        assert!(after_ws.contains("[x] Whitespace changes"));
        assert!(after_ws.contains("ws_only.txt"));

        // Move down to Untracked files and toggle with space.
        app.press("down");
        app.press("space");
        let after_u = capture_text(&app.render());
        assert!(after_u.contains("[ ] Untracked files"));
        assert!(!after_u.contains("untracked.txt"));

        // Close modal with 'c'.
        app.press("c");
        let closed = capture_text(&app.render());
        assert!(!closed.contains("Whitespace changes"));
    }

    #[test]
    fn edit_shortcut_and_footer_button_open_active_file_and_reload() {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let edited_marker = Rc::new(std::cell::Cell::new(false));

        let edited_for_loader = Rc::clone(&edited_marker);
        let loader: DiffLoader = Rc::new(move |opts| {
            let mut files = sample_files(opts);
            if edited_for_loader.get()
                && let Some(alpha) = files.iter_mut().find(|f| f.path == "src/alpha.rs")
            {
                alpha.lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    text: "+edited in $EDITOR".to_owned(),
                });
            }
            Ok(LoadedDiff {
                files,
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            })
        });

        let opened_for_cb = Rc::clone(&opened);
        let edited_for_cb = Rc::clone(&edited_marker);
        let opener: FileOpener = Rc::new(move |path| {
            opened_for_cb.borrow_mut().push(path.to_owned());
            edited_for_cb.set(true);
            Ok(())
        });

        let state = AppState::new(
            LoadedDiff {
                files: sample_files(&DiffOptions::default()),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        mount(&app, state, loader, opener, noop_stager(), noop_unstager());

        let capture = app.render();
        let footer_y = (capture.height - 1) as u16;
        let footer_row = capture.row(capture.height - 1);
        assert!(footer_row.contains(" e edit "), "footer should show edit button: {footer_row}");

        // Press 'e' to edit active file (src/alpha.rs); diff reloads with the updated line.
        app.press("e");
        let after_key = capture_text(&app.render());
        assert_eq!(*opened.borrow(), vec!["src/alpha.rs"]);
        assert!(after_key.contains("+edited in $EDITOR"));

        // Move to src/beta.rs and click the footer edit button.
        app.press("n");
        let capture = app.render();
        let footer_row = capture.row(capture.height - 1);
        let edit_x = footer_row.find(" e edit ").expect("edit button") as u16 + 2;
        app.click(edit_x, footer_y);
        let _ = app.render();
        assert_eq!(*opened.borrow(), vec!["src/alpha.rs", "src/beta.rs"]);
    }

    #[test]
    fn edit_error_is_shown_in_footer_and_noop_when_no_active_file() {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let calls = Rc::new(std::cell::Cell::new(0usize));
        let calls_cb = Rc::clone(&calls);
        let opener: FileOpener = Rc::new(move |_path| {
            calls_cb.set(calls_cb.get() + 1);
            Err("$EDITOR is not set".to_owned())
        });
        let files = vec![DiffFile {
            path: "only.rs".to_owned(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 1,
            deletions: 0,
            is_binary: false,
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                text: "+line".to_owned(),
            }],
        }];
        let staged_flag = Rc::new(std::cell::Cell::new(false));
        let staged_for_loader = Rc::clone(&staged_flag);
        let loader_files = files.clone();
        let loader: DiffLoader = Rc::new(move |_opts| {
            if staged_for_loader.get() {
                Ok(LoadedDiff {
                    files: Vec::new(),
                    staged: vec![StagedFile {
                        path: "only.rs".to_owned(),
                        old_path: None,
                        status: FileStatus::Modified,
                        worktree_status: None,
                    }],
                    staged_diffs: Vec::new(),
                })
            } else {
                Ok(LoadedDiff {
                    files: loader_files.clone(),
                    staged: Vec::new(),
                    staged_diffs: Vec::new(),
                })
            }
        });
        let staged_for_stager = Rc::clone(&staged_flag);
        let stager: FileStager = Rc::new(move |_path, _old| {
            staged_for_stager.set(true);
            Ok(())
        });
        let state = AppState::new(
            LoadedDiff {
                files: files.clone(),
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            },
            DiffTarget::WorkingTree,
        );
        mount(&app, state, loader, opener, stager, noop_unstager());

        app.press("e");
        let screen = capture_text(&app.render());
        assert_eq!(calls.get(), 1);
        assert!(screen.contains("error: $EDITOR is not set"));

        // Stage the only file so active_file is None; pressing 'e' does nothing.
        app.press("s");
        let _ = app.render();
        app.press("e");
        let _ = app.render();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn pressing_k_at_top_does_not_delay_subsequent_j_scrolling() {
        let app = TestApp::with_options(
            100,
            10,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let mut lines = vec![DiffLine {
            kind: DiffLineKind::HunkHeader,
            text: "@@ -1,1 +1,30 @@".to_owned(),
        }];
        for i in 0..30 {
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: format!("+line_{i:02}"),
            });
        }
        let files = vec![DiffFile {
            path: "src/tall.rs".to_owned(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 30,
            deletions: 0,
            is_binary: false,
            lines,
        }];
        let loaded = LoadedDiff {
            files,
            staged: Vec::new(),
            staged_diffs: Vec::new(),
        };
        let state = AppState::new(loaded.clone(), DiffTarget::WorkingTree);
        let loader: DiffLoader = Rc::new(move |_opts| Ok(loaded.clone()));
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

        let initial = capture_text(&app.render());
        assert!(initial.contains("M src/tall.rs"));

        // Press 'k' many times while already at the top of the diff view.
        for _ in 0..25 {
            app.press("k");
        }
        let _ = app.render();

        // A few 'j' presses should immediately scroll the header off the top of the viewport.
        for _ in 0..4 {
            app.press("j");
        }
        let after_j = capture_text(&app.render());
        assert!(
            !after_j.contains("M src/tall.rs"),
            "scrolling down with 'j' after pressing 'k' at top should take effect immediately:\n{after_j}"
        );
    }

    #[test]
    fn refresh_shortcut_and_footer_button_reload_all_views() {
        let app = TestApp::with_options(
            120,
            30,
            AppOptions::default().with_theme(Theme::terminal()),
        );
        let revision = Rc::new(std::cell::Cell::new(0u8));
        let rev_for_loader = Rc::clone(&revision);

        let loader: DiffLoader = Rc::new(move |_opts| match rev_for_loader.get() {
            0 => Ok(LoadedDiff {
                files: vec![DiffFile {
                    path: "src/alpha.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        text: "+initial alpha".to_owned(),
                    }],
                }],
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            }),
            1 => Ok(LoadedDiff {
                files: vec![
                    DiffFile {
                        path: "src/alpha.rs".to_owned(),
                        old_path: None,
                        status: FileStatus::Modified,
                        additions: 1,
                        deletions: 0,
                        is_binary: false,
                        lines: vec![DiffLine {
                            kind: DiffLineKind::Added,
                            text: "+updated after r".to_owned(),
                        }],
                    },
                    DiffFile {
                        path: "src/new_file.rs".to_owned(),
                        old_path: None,
                        status: FileStatus::Untracked,
                        additions: 1,
                        deletions: 0,
                        is_binary: false,
                        lines: vec![DiffLine {
                            kind: DiffLineKind::Added,
                            text: "+brand new".to_owned(),
                        }],
                    },
                ],
                staged: vec![StagedFile {
                    path: "staged_ext.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Added,
                    worktree_status: None,
                }],
                staged_diffs: Vec::new(),
            }),
            2 => Err("git refresh failed".to_owned()),
            _ => Ok(LoadedDiff {
                files: vec![DiffFile {
                    path: "src/alpha.rs".to_owned(),
                    old_path: None,
                    status: FileStatus::Modified,
                    additions: 1,
                    deletions: 0,
                    is_binary: false,
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        text: "+updated after button click".to_owned(),
                    }],
                }],
                staged: Vec::new(),
                staged_diffs: Vec::new(),
            }),
        });

        let initial = loader(&DiffOptions::default()).unwrap();
        let state = AppState::new(initial, DiffTarget::WorkingTree);
        mount(&app, state, loader, noop_opener(), noop_stager(), noop_unstager());

        let initial_capture = app.render();
        let initial_screen = capture_text(&initial_capture);
        assert!(initial_screen.contains("+initial alpha"));
        assert!(initial_screen.contains("Changed (1)"));
        assert!(!initial_screen.contains("Staged ("));

        // External changes happen; pressing 'r' reloads both sidebar and main diff.
        revision.set(1);
        app.press("r");
        let after_r = capture_text(&app.render());
        assert!(after_r.contains("+updated after r"), "main diff should refresh:\n{after_r}");
        assert!(after_r.contains("new_file.rs"), "new file should appear in sidebar:\n{after_r}");
        assert!(after_r.contains("Staged (1)"), "staged section should appear:\n{after_r}");
        assert!(after_r.contains("staged_ext.rs"), "staged file should appear:\n{after_r}");
        assert!(after_r.contains("Changed (2)"), "changed count should update:\n{after_r}");

        // Refresh error is shown in footer when pressing 'R'.
        revision.set(2);
        app.press("R");
        let after_err = capture_text(&app.render());
        assert!(after_err.contains("error: git refresh failed"));

        // Clicking the ' r refresh ' footer button reloads again and clears the error.
        revision.set(3);
        let capture = app.render();
        let footer_y = (capture.height - 1) as u16;
        let footer_row = capture.row(capture.height - 1);
        let refresh_x = footer_row.find(" r refresh ").expect("refresh button") as u16 + 2;
        app.click(refresh_x, footer_y);
        let after_click = capture_text(&app.render());
        assert!(after_click.contains("+updated after button click"));
        assert!(!after_click.contains("error: git refresh failed"));
        assert!(!after_click.contains("Staged ("));
        assert!(after_click.contains("Changed (1)"));
    }
}
