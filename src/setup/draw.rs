//! Draw setup in a full tab or in a narrow rail without hiding the cursor.

use std::ffi::OsStr;

use crate::git::Kind;
use crate::help;
use crate::icons;
use crate::preferences::Source;
use crate::screen::{self, Screen, Style};
use crate::ui::{self, ADD, FAINT, HEADING};
use crate::width::char_width;

use super::preview::{Preview, Rect, pane_label};
use super::{Claude, Confirm, Panel, Row, SaveKind, Scope, Setup, Side};

const ACCENT: Style = Style::fg(screen::CYAN);

#[derive(Default)]
pub(super) struct Visible {
    first: usize,
    y: usize,
    width: usize,
    pub(super) len: usize,
}

impl Visible {
    pub(super) fn index_at(&self, x: usize, y: usize) -> Option<usize> {
        let offset = y.checked_sub(self.y)?;
        (x < self.width && offset < self.len).then_some(self.first + offset)
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    pub(super) fn draw_setup(&mut self, s: &mut Screen) {
        s.clear();
        self.visible = Visible::default();
        if s.w < 4 || s.h == 0 {
            return;
        }
        ui::section_heading(s, 0, s.w - 1, "Milo setup");
        if s.h == 1 {
            return;
        }
        self.footer(s);
        let end = self.notice(s);
        let tab = self.panel.tab();
        let mut y = 1;
        if let Some(active) = tab {
            if s.w >= 60 {
                tabs(s, 15, 0, active);
            } else if end >= 5 {
                tabs(s, 1, y, active);
                y += 1;
            }
        }
        if end >= y + 2 {
            y += usize::from(end >= 12);
            self.title(s, y);
            y += 1 + usize::from(end >= 12);
        }
        if self.panel == Panel::Diagnostics {
            self.draw_diagnostics(s, y, end);
            return;
        }

        let rows = self.rows();
        let width = s.w.saturating_sub(2);
        if let Some(text) = self.intro() {
            let spare = end.saturating_sub(y + rows.len().min(3));
            for line in wrap(text, width).take(spare) {
                s.put(1, y, width, line, FAINT);
                y += 1;
            }
        }
        let preview = (self.inside && matches!(self.panel, Panel::Layout | Panel::Tune))
            .then(|| Preview::new(&self.layout, self.tab_size));
        let side_by_side = preview.is_some() && s.w >= 76;
        let form_width = if side_by_side { 32 } else { s.w.min(54) };
        let mut form_end = end;
        let mut diagram = None;
        let mut sample = None;
        if let Some(preview) = &preview {
            if side_by_side {
                let preview_width = (s.w - form_width - 5).min(48);
                diagram = Some(Rect {
                    x: form_width + 4,
                    y,
                    width: preview_width,
                    height: preview.height(preview_width, end.saturating_sub(y + 1).min(10)),
                });
            } else if s.w >= 16 && end.saturating_sub(y) >= 6 + preview.minimum_height() {
                let height = preview.height(width, (end - y - 6).min(10));
                let top = end - height - 1;
                form_end = top - 1;
                diagram = Some(Rect { x: 1, y: top, width, height });
            }
        } else if matches!(self.panel, Panel::Appearance | Panel::Workflow) && end.saturating_sub(y) >= 13 {
            form_end = end - 5;
            sample = Some(form_end + 1);
        }

        let visible = self.draw_rows(s, y, form_end, form_width);
        let next = visible.y + visible.len;
        if let Some(&row) = rows.get(self.selected)
            && next + 1 < form_end
        {
            let cell = self.cell(row);
            let detail = self.detail(row, cell);
            for (i, line) in wrap(detail, form_width.saturating_sub(3)).take(form_end - next - 1).enumerate() {
                s.put(2, next + 1 + i, form_width - 3, line, FAINT);
            }
        }
        self.visible = visible;

        if let (Some(Rect { x, y, width, height }), Some(preview)) = (diagram, preview)
            && y < end
        {
            preview.caption(s, x, y, width, !self.installed || self.saved.as_ref() != Some(&self.layout));
            let highlight = match rows.get(self.selected) {
                Some(Row::Pane(pane)) => Some(*pane),
                Some(Row::Order(at)) => Some(self.layout.geometry.order[*at]),
                _ => None,
            };
            preview.draw(s, Rect { x, y: y + 1, width, height }, highlight);
        }
        if let Some(top) = sample {
            if self.panel == Panel::Appearance {
                self.appearance_sample(s, top);
            } else {
                self.workflow_sample(s, top);
            }
        }
    }

    fn notice(&self, s: &mut Screen) -> usize {
        let end = s.h - 1;
        let Some((text, style)) = self.message.as_ref() else { return end };
        let width = s.w.saturating_sub(2).max(1);
        let lines = wrap(text, width).take((s.h / 3).min(end.saturating_sub(2)));
        let top = end - lines.clone().count();
        for (i, line) in lines.enumerate() {
            s.put(1, top + i, width, line, *style);
        }
        top
    }

    fn title(&self, s: &mut Screen, y: usize) {
        let show_heading = !matches!(self.panel, Panel::Layout | Panel::Appearance | Panel::Workflow) || !self.inside;
        let at = if show_heading {
            let heading = self.panel.title();
            ui::section_heading(s, y, s.w - 2, heading);
            heading.len() + 3
        } else {
            1
        };
        let scope = match self.panel {
            Panel::Layout | Panel::Tune if self.inside => {
                if self.scope == Scope::Worktree {
                    "This worktree"
                } else {
                    "Global default"
                }
            }
            Panel::Appearance | Panel::Workflow if self.saved_prefs.is_none() => "Preferences not loaded",
            Panel::Appearance | Panel::Workflow => match self.prefs_scope {
                Source::Default => "Built-in defaults",
                Source::Global => "Global preferences",
                Source::Worktree => "Worktree preferences",
            },
            Panel::Health if !self.inside => "Outside Rex",
            Panel::Health if self.saved.is_none() => "Rex settings unreadable",
            Panel::Health if self.installed => "Rex integration installed",
            Panel::Health => "Rex not set up yet",
            _ => return,
        };
        if at + scope.len() < s.w {
            let used = s.put(at, y, scope.len(), scope, FAINT);
            let at = at + used + 2;
            let scoped_to_worktree = matches!(self.panel, Panel::Layout | Panel::Tune) && self.scope == Scope::Worktree
                || matches!(self.panel, Panel::Appearance | Panel::Workflow) && self.prefs_scope == Source::Worktree;
            if scoped_to_worktree
                && let Some(name) = self.root.as_deref().and_then(|root| root.file_name()).and_then(OsStr::to_str)
            {
                s.put_fit(at, y, s.w.saturating_sub(at + 1), name, FAINT);
            }
        }
    }

    fn intro(&self) -> Option<&'static str> {
        Some(match self.panel {
            Panel::Layout if !self.inside => {
                "Open setup in Rex to arrange tabs. The panes work anywhere: milo outline, milo history, milo usage."
            }
            Panel::Layout if self.trial.as_ref().is_some_and(|(trial, _)| trial.temporary()) => {
                "Temporary preview only. Undo it now, or save the draft for future tabs."
            }
            Panel::Layout if self.trial.is_some() => "Undo this trial, or save the layout you tried to keep its panes.",
            Panel::Layout => return None,
            Panel::Tune => return None,
            Panel::Appearance => return None,
            Panel::Workflow => return None,
            Panel::Health => "Inspect settings without changing them. Repair or remove only Milo's integration.",
            Panel::Diagnostics => return None,
            Panel::Save(SaveKind::Layout) => {
                "On save: install or update Milo's Rex actions. Auto-arrange changes only with a global save."
            }
            Panel::Save(_) => "On save: remember these appearance and workflow choices. Reopen Milo panes to apply.",
            Panel::Confirm(Confirm::UseGlobal) => {
                "Remove this worktree's saved layout override and discard the layout draft. The global default takes over."
            }
            Panel::Confirm(Confirm::UseGlobalPrefs(_)) => {
                "Remove this worktree's appearance and workflow override and discard the preference draft."
            }
            Panel::Confirm(Confirm::Repair) => {
                "Repair Milo's saved Rex integration without changing the layout choices. Rex may reload."
            }
            Panel::Confirm(Confirm::Remove) => {
                "Remove integration Milo owns from Rex and Claude Code. Open panes and personal preferences are left alone."
            }
        })
    }

    fn footer(&self, s: &mut Screen) {
        let back = matches!(self.panel, Panel::Tune | Panel::Save(_) | Panel::Confirm(_) | Panel::Diagnostics);
        let exit = if back {
            "Esc back"
        } else if self.offered {
            "q next"
        } else {
            "q quit"
        };
        if s.w < 20 {
            s.put_fit(1, s.h - 1, s.w - 1, exit, FAINT);
            return;
        }
        if self.panel == Panel::Layout && self.inside && self.saved.is_some() && (24..36).contains(&s.w) {
            let trial = if self.trial.is_some() { "u undo" } else { "a try" };
            let x = 1 + s.put(1, s.h - 1, 6, trial, FAINT);
            let x = x + s.put(x, s.h - 1, 10, "  s save  ", FAINT);
            s.put_fit(x, s.h - 1, s.w - x, exit, FAINT);
            return;
        }
        let last = if back {
            "Esc back · ? help"
        } else if self.offered {
            "q next · ? help"
        } else {
            "q quit · ? help"
        };
        let (first, middle): (&str, &[&str]) = match self.panel {
            Panel::Diagnostics => ("↑↓ scroll", &["r refresh"]),
            Panel::Save(_) | Panel::Confirm(_) => ("Enter choose", &[]),
            Panel::Layout | Panel::Tune if self.inside && self.saved.is_some() => {
                ("s save…", if self.trial.is_some() { &["u undo", "Tab sections"] } else { &["a try", "Tab sections"] })
            }
            Panel::Appearance | Panel::Workflow if self.saved_prefs.is_some() => ("s save…", &["Enter", "Tab section"]),
            _ => ("Enter", &["Tab section"]),
        };
        help::draw_hint(s, first, middle, last);
    }

    fn draw_rows(&self, s: &mut Screen, y: usize, end: usize, width: usize) -> Visible {
        let rows = self.rows();
        let len = end.saturating_sub(y).min(rows.len());
        let first = self.selected.saturating_sub(len.saturating_sub(1)).min(rows.len() - len);
        let draw_width = width.saturating_sub(1);
        for (i, &row) in rows[first..first + len].iter().enumerate() {
            let row_y = y + i;
            let cell = self.cell(row);
            let selected = first + i == self.selected;
            let disabled = self.disabled(row);
            if selected {
                s.put(0, row_y, 1, "›", if disabled { FAINT } else { ACCENT.bold() });
            }
            let (label, name_style) = (
                if width >= 42 { cell.label } else { cell.short },
                if disabled {
                    FAINT
                } else if selected {
                    HEADING
                } else {
                    Style::PLAIN
                },
            );
            if let Some((value, style)) = cell.value {
                let room = (draw_width.saturating_sub(4) / 2).min(13);
                let start = draw_width.saturating_sub(room);
                s.put_fit(2, row_y, start.saturating_sub(3), label, name_style);
                s.put_fit(start, row_y, room, value, if disabled { FAINT } else { style });
            } else {
                s.put_fit(2, row_y, draw_width.saturating_sub(2), label, name_style);
            }
        }
        ui::thumb(s, draw_width, y, len, rows.len(), first);
        Visible { first, y, width, len }
    }

    fn draw_diagnostics(&mut self, s: &mut Screen, y: usize, end: usize) {
        let width = s.w.saturating_sub(3).max(1);
        let text = if self.health.is_empty() { "No diagnostics were returned." } else { self.health.as_str() };
        let lines = wrap(text, width);
        let len = lines.clone().count();
        let shown = end.saturating_sub(y);
        self.health_top = self.health_top.min(len.saturating_sub(shown));
        for (i, line) in lines.skip(self.health_top).take(shown).enumerate() {
            s.put(1, y + i, width, line, Style::PLAIN);
        }
        self.visible.len = shown;
        ui::thumb(s, s.w - 1, y, shown, len, self.health_top);
    }

    fn appearance_sample(&self, s: &mut Screen, y: usize) {
        let mode = self.icon_override.unwrap_or(self.prefs.icons);
        let title = if self.icon_override.is_some() { "Sample (MILO_ICONS override)" } else { "Sample" };
        ui::section_heading(s, y, s.w - 2, title);
        let end = s.w - 1;
        let used = ui::icon(s, 2, y + 1, end.saturating_sub(2), icons::dir(mode, true));
        s.put_fit(2 + used, y + 1, end.saturating_sub(2 + used), "src", Style::PLAIN);
        let changed = ui::kind_style(Kind::Modified).bold();
        let mut x = 4;
        s.put(2, y + 2, 1, if self.prefs.static_marks { "•" } else { "●" }, changed);
        if self.prefs.text_git_marks {
            x += ui::git_mark(s, x, y + 2, end.saturating_sub(x), Kind::Modified, true);
        }
        x += ui::icon(s, x, y + 2, end.saturating_sub(x), icons::file(mode, "main.rs"));
        s.put_fit(x, y + 2, end.saturating_sub(x), "main.rs", changed);
        let usage = if self.prefs.usage_large_figures { "12%  Claude 5h" } else { "Claude 5h · 12% left" };
        s.put_fit(2, y + 3, end.saturating_sub(2), usage, FAINT);
    }

    fn workflow_sample(&self, s: &mut Screen, y: usize) {
        let end = s.w - 2;
        ui::section_heading(s, y, end, "What panes will do");
        let project = if self.prefs.project_mode == super::ProjectMode::Pin || !self.inside {
            "Keep the starting project open."
        } else {
            "Follow this shell when Rex reports its folder."
        };
        let outline = match (self.prefs.changes_only, self.prefs.follow_activity) {
            (false, false) => "Outline: all files, stays put.",
            (false, true) => "Outline: all files, follows edits.",
            (true, false) => "Outline: changes, stays put.",
            (true, true) => "Outline: changes, follows edits.",
        };
        s.put_fit(1, y + 1, end, project, FAINT);
        s.put_fit(1, y + 2, end, outline, FAINT);
        s.put_fit(1, y + 3, end, "Folder choices are remembered per worktree.", FAINT);
    }

    fn detail<'a>(&'a self, row: Row, cell: Cell<'a>) -> &'a str {
        match row {
            Row::Capture if self.trial.is_some() => "Undo the current trial first.",
            Row::Capture if self.target.is_none() => "Open setup from a tiled Rex shell to use this action.",
            Row::SaveWorktree => self.root.as_deref().and_then(|root| root.to_str()).unwrap_or(cell.detail),
            Row::ClaudeLimits | Row::SaveClaude => match self.claude {
                Claude::Foreign => "Milo will leave your Claude Code status line alone.",
                Claude::NotOwned => "You already added Milo to this Claude Code status line.",
                Claude::Invalid => "Could not read Claude Code settings. Milo will leave them alone.",
                Claude::On | Claude::Off => cell.detail,
            },
            Row::Icons if self.icon_override.is_some() => "MILO_ICONS overrides this saved icon choice.",
            Row::SaveGlobal if self.panel == Panel::Save(SaveKind::Layout) && self.scope == Scope::Worktree => {
                "Updates the global default without changing this worktree's override."
            }
            Row::SaveGlobal
                if matches!(self.panel, Panel::Save(SaveKind::Appearance | SaveKind::Workflow))
                    && self.prefs_scope == Source::Worktree =>
            {
                "Updates global preferences without changing this worktree's override."
            }
            _ => cell.detail,
        }
    }

    fn cell(&self, row: Row) -> Cell<'_> {
        let layout = &self.layout;
        let prefs = &self.prefs;
        match row {
            Row::Tree => Cell::action("Just the tree", "Just the tree", "Browse files beside your shell."),
            Row::Sidebar => {
                Cell::action("Project sidebar", "Project sidebar", "Files, commits and usage beside your shell.")
            }
            Row::Capture => Cell::action(
                "Use this tab’s layout",
                "Use this tab’s layout",
                "Read Milo's side, order and split sizes without moving anything.",
            ),
            Row::Tune => Cell::action("Customize panes", "Customize panes", "Choose the panes, side, order and size."),
            Row::Undo => Cell::action("Undo trial", "Undo trial", "Close only the panes created by this trial."),
            Row::SaveLayout => Cell::action(
                "Save layout for…",
                "Save layout for…",
                "Choose a worktree override or the global default.",
            ),
            Row::UseGlobal => {
                Cell::action("Use global default", "Use global default", "Remove this worktree's layout override.")
            }
            Row::Pane(pane) => Cell::toggle(
                pane_label(pane),
                pane_label(pane),
                layout.chosen(pane),
                "Show or hide this pane in the draft.",
            ),
            Row::Side => Cell::choice(
                "Rail side",
                "Rail side",
                if layout.geometry.side == Side::Left { "Left" } else { "Right" },
                "Which side of your shell holds Milo panes.",
            ),
            Row::Width => Cell::choice(
                "Rail width",
                "Rail width",
                layout.width_name(),
                "Cycle relative widths. The preview shows the draft percentage.",
            ),
            Row::Order(index) => {
                let label = ["First position", "Second position", "Third position"][index];
                Cell::choice(
                    label,
                    label,
                    pane_label(layout.geometry.order[index]),
                    "Swap the role in this position. Hidden panes are skipped.",
                )
            }
            Row::OutlineShare => {
                let value =
                    if layout.geometry.shares.is_some() { "Captured" } else { fraction_label(&layout.outline_share) };
                Cell::choice(
                    "Outline share",
                    "Outline share",
                    value,
                    "Outline's share above or below History, excluding Usage. Changing this replaces captured heights.",
                )
            }
            Row::UsageRows => {
                let value = if layout.geometry.shares.is_some() { "Captured" } else { &layout.usage_rows };
                Cell::choice(
                    "Usage rows",
                    "Usage rows",
                    value,
                    "Target rows for Usage, capped at 40% of a shared rail. Changing this replaces captured heights.",
                )
            }
            Row::AutoArrange => Cell::toggle(
                "Arrange new shell tabs",
                "Auto-arrange",
                layout.new_tabs,
                "Global only. New command tabs and tabs with a layout are skipped.",
            ),
            Row::Back | Row::Cancel => Cell::action("Back", "Back", "Return without writing settings."),
            Row::Icons => Cell::choice(
                "Icons",
                "Icons",
                prefs.icons.label(),
                "Choose Nerd Font symbols, plain symbols, or no icons.",
            ),
            Row::Activity => Cell::choice(
                "Activity marks",
                "Activity marks",
                if prefs.static_marks { "Static" } else { "Pulse" },
                "Show recent edits with a still or pulsing marker.",
            ),
            Row::GitMarks => Cell::choice(
                "Git status marks",
                "Git marks",
                if prefs.text_git_marks { "Text" } else { "Style" },
                "Text adds letters and signs beside changed files and commits.",
            ),
            Row::UsageStyle => Cell::choice(
                "Usage display",
                "Usage display",
                if prefs.usage_large_figures { "Large figures" } else { "Auto compact" },
                "Auto uses text in narrow panes and large figures where there is room.",
            ),
            Row::FollowProject => Cell::choice(
                "Project tracking",
                "Project",
                prefs.project_mode.label(),
                "Follow the originating shell, or stay with the starting project.",
            ),
            Row::FollowActivity => Cell::toggle(
                "Follow file edits",
                "Follow edits",
                prefs.follow_activity,
                "Reveal active files without remembering temporary reveals.",
            ),
            Row::ChangesOnly => Cell::choice(
                "Outline starts with",
                "Outline starts",
                if prefs.changes_only { "Changes" } else { "All files" },
                "Set the initial Outline view. Press c in Outline to switch.",
            ),
            Row::HistoryFollow => Cell::toggle(
                "Follow new commits",
                "New commits",
                prefs.history_follow_new,
                "Keep History at the newest commit while you are at the top.",
            ),
            Row::ClaudeLimits => {
                if self.claude.editable() {
                    Cell::toggle(
                        "Show Claude Code limits",
                        "Claude limits",
                        self.show_limits,
                        "Use Milo's Claude Code status line to read limits.",
                    )
                } else {
                    Cell::choice("Show Claude Code limits", "Claude limits", "Unchanged", "")
                }
            }
            Row::SavePrefs => Cell::action(
                "Save pane preferences for…",
                "Save preferences…",
                "Save the appearance and workflow draft.",
            ),
            Row::UseGlobalPrefs => Cell::action(
                "Use global pane preferences",
                "Use global prefs",
                "Remove this worktree's pane preferences override.",
            ),
            Row::SaveClaude => Cell::action(
                "Save Claude Code choices",
                "Save Claude choices",
                "Only changes Claude Code's status line, if Milo owns it or it is unused.",
            ),
            Row::Diagnostics => Cell::action(
                "Read diagnostics",
                "Read diagnostics",
                "Check preference scope, Rex, auto-arrange and usage data sources.",
            ),
            Row::Repair => Cell::action(
                "Repair integration…",
                "Repair integration…",
                "Update Milo's Rex launcher and restart auto-arrange if needed.",
            ),
            Row::Remove => Cell::action(
                "Remove Milo integration…",
                "Remove integration…",
                "Remove only the integrations Milo manages. Nothing is removed until you confirm.",
            ),
            Row::SaveGlobal => Cell::action(
                "As global default",
                "As global default",
                "Save for tabs and panes without a worktree override.",
            ),
            Row::SaveWorktree => {
                Cell::action("For this worktree", "For this worktree", "Save only for the current Git worktree.")
            }
            Row::Continue => match self.panel {
                Panel::Confirm(Confirm::UseGlobal | Confirm::UseGlobalPrefs(_)) => {
                    Cell::action("Remove override", "Remove override", "Use the global setting from now on.")
                }
                Panel::Confirm(Confirm::Repair) => {
                    Cell::action("Repair integration", "Repair integration", "Apply the repair now.")
                }
                Panel::Confirm(Confirm::Remove) => {
                    Cell::action("Remove integration", "Remove integration", "Apply removal now.")
                }
                _ => unreachable!("only confirmation pages have a continue row"),
            },
        }
    }
}

impl Panel {
    fn tab(self) -> Option<usize> {
        match self {
            Self::Layout | Self::Tune => Some(0),
            Self::Appearance => Some(1),
            Self::Workflow => Some(2),
            Self::Health | Self::Diagnostics => Some(3),
            Self::Save(_) | Self::Confirm(_) => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Layout => "Layout",
            Self::Tune => "Customize panes",
            Self::Appearance => "Appearance",
            Self::Workflow => "Workflow",
            Self::Health => "Health",
            Self::Diagnostics => "Diagnostics",
            Self::Save(SaveKind::Layout) => "Save layout for…",
            Self::Save(_) => "Save preferences for…",
            Self::Confirm(Confirm::UseGlobal | Confirm::UseGlobalPrefs(_)) => "Use global default?",
            Self::Confirm(Confirm::Repair) => "Repair integration?",
            Self::Confirm(Confirm::Remove) => "Remove integration?",
        }
    }
}

#[derive(Clone, Copy)]
struct Cell<'a> {
    label: &'static str,
    short: &'static str,
    value: Option<(&'a str, Style)>,
    detail: &'static str,
}

impl<'a> Cell<'a> {
    fn action(label: &'static str, short: &'static str, detail: &'static str) -> Self {
        Self { label, short, value: None, detail }
    }

    fn choice(label: &'static str, short: &'static str, value: &'a str, detail: &'static str) -> Self {
        Self { label, short, value: Some((value, ACCENT)), detail }
    }

    fn toggle(label: &'static str, short: &'static str, on: bool, detail: &'static str) -> Self {
        Self { label, short, value: Some(if on { ("On", ADD) } else { ("Off", FAINT) }), detail }
    }
}

fn tabs(s: &mut Screen, mut x: usize, y: usize, active: usize) {
    let labels = ["Layout", "Appearance", "Workflow", "Health"];
    if s.w < 37 {
        s.put_fit(x, y, s.w.saturating_sub(x), labels[active], HEADING);
        return;
    }
    let spacing = 2;
    for (index, text) in labels.into_iter().enumerate() {
        let style = if index == active { ACCENT.bold() } else { FAINT };
        x += s.put(x, y, text.len(), text, style) + spacing;
    }
}

fn fraction_label(part: &str) -> &str {
    match part {
        "0.50" => "50%",
        "0.60" => "60%",
        "0.70" => "70%",
        "0.80" => "80%",
        _ => part,
    }
}

/// Wrap whole words, but make forward progress even on a too-wide glyph.
fn wrap(text: &str, width: usize) -> impl Iterator<Item = &str> + Clone {
    text.lines().flat_map(move |mut text| {
        let mut blank = text.is_empty();
        std::iter::from_fn(move || {
            if std::mem::take(&mut blank) {
                return Some("");
            }
            if width == 0 || text.is_empty() {
                return None;
            }
            let mut end = text.len();
            let mut columns = 0;
            let mut space = None;
            for (i, ch) in text.char_indices() {
                columns += char_width(ch);
                if columns > width {
                    end = if i == 0 {
                        ch.len_utf8()
                    } else if ch == ' ' {
                        i
                    } else {
                        space.unwrap_or(i)
                    };
                    break;
                }
                if ch == ' ' && i != 0 {
                    space = Some(i);
                }
            }
            let (line, rest) = text.split_at(end);
            text = rest.trim_start();
            Some(line)
        })
    })
}
