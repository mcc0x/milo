//! Draw setup in a full tab or in a narrow rail without hiding the cursor.

use std::ffi::OsStr;
use std::ops::Range;

use crate::alerts;
use crate::git::Kind;
use crate::help;
use crate::icons;
use crate::keys::Key;
use crate::preferences::Source;
use crate::screen::{self, Screen, Style};
use crate::ui::{self, ADD, FAINT, HEADING};
use crate::width::{char_width, str_width};

use super::preview::{Preview, Rect, pane_label};
use super::{Claude, Confirm, Panel, Row, SECTIONS, SaveKind, Scope, Setup, Side, TUNE};

const ACCENT: Style = Style::fg(screen::CYAN);
const UNSAVED: Style = Style::fg(screen::YELLOW).bold();
const NARROW: usize = 24;

#[derive(Default)]
struct RowHit {
    marker: bool,
    label: Range<usize>,
    value: Range<usize>,
}

impl RowHit {
    fn span_at(&self, x: usize) -> Option<Range<usize>> {
        if self.marker && x == 0 {
            Some(0..1)
        } else if self.label.contains(&x) {
            Some(self.label.clone())
        } else if self.value.contains(&x) {
            Some(self.value.clone())
        } else {
            None
        }
    }
}

/// Each row on screen has a place in `Visible::rows`: enough for the longest list.
const ROWS: usize = if super::WORKFLOW.len() > TUNE.len() { super::WORKFLOW.len() } else { TUNE.len() };

#[derive(Default)]
pub(super) struct Visible {
    current: bool,
    first: usize,
    y: usize,
    pub(super) len: usize,
    pub(super) scroll_limit: usize,
    rows: [RowHit; ROWS],
    tab_y: usize,
    tabs: [Range<usize>; SECTIONS.len()],
    footer: help::HintHits<'static>,
}

impl Visible {
    pub(super) fn invalidate(&mut self) {
        self.current = false;
    }

    pub(super) fn row_at(&self, x: usize, y: usize) -> Option<(usize, Range<usize>)> {
        let offset = y.checked_sub(self.y)?;
        if !self.current || offset >= self.len {
            return None;
        }
        let span = self.rows.get(offset)?.span_at(x)?;
        Some((self.first + offset, span))
    }

    pub(super) fn tab_at(&self, x: usize, y: usize) -> Option<(Panel, Range<usize>)> {
        if !self.current || y != self.tab_y {
            return None;
        }
        self.tabs
            .iter()
            .enumerate()
            .find_map(|(index, span)| span.contains(&x).then(|| (SECTIONS[index], span.clone())))
    }

    pub(super) fn footer_at(&self, x: usize, y: usize) -> Option<Key> {
        if !self.current {
            return None;
        }
        let label = self.footer.item_at(x, y)?;
        Some(match label.chars().next()? {
            'E' | '‹' | 'b' => Key::Esc,
            'T' => Key::Ctrl(b'i'),
            't' => Key::Char('a'),
            '↑' => Key::Up,
            '↓' => Key::Down,
            ch => Key::Char(ch),
        })
    }

    pub(super) fn footer_span_at(&self, x: usize, y: usize) -> Option<Range<usize>> {
        if !self.current {
            return None;
        }
        self.footer.span_at(x, y)
    }
}

impl<R: Fn(&[&OsStr]) -> bool> Setup<R> {
    pub(super) fn draw_setup(&mut self, s: &mut Screen) {
        s.clear();
        self.visible = Visible::default();
        if s.w == 0 || s.h == 0 {
            return;
        }
        let dirty = self.dirty();
        if s.h == 1 || s.w < 4 {
            if self.panel == Panel::Diagnostics {
                self.draw_diagnostics(s, 0, s.h);
            } else if !self.rows().is_empty() {
                self.visible.current = true;
                self.draw_rows(s, 0, s.h, s.w.min(54));
            } else if s.w >= 4 {
                self.heading(s, dirty);
            }
            return;
        }
        self.visible.current = true;
        self.visible.footer = self.footer(s);
        let end = self.notice(s);
        if end == 1 && self.panel == Panel::Diagnostics {
            self.draw_diagnostics(s, 0, end);
            return;
        }
        if end == 1 && !self.rows().is_empty() {
            self.draw_rows(s, 0, end, s.w.min(54));
            return;
        }
        let mut y = 1;
        let tab = self.panel.tab();
        // Keep a content row in short rails by using the heading for tabs.
        let tabs_in_heading = tab.is_some() && s.w < 60 && end < 3;
        if !tabs_in_heading {
            self.heading(s, dirty);
        }
        if let Some(active) = tab {
            let marker = tabs_in_heading && dirty && s.w > SECTIONS.len();
            let end = s.w - usize::from(marker);
            let (x, tab_y) = if s.w >= 60 {
                (if dirty { 19 } else { 15 }, 0)
            } else if tabs_in_heading {
                (usize::from(end >= 8), 0)
            } else {
                y += 1;
                (usize::from(end >= 8), 1)
            };
            self.visible.tabs = tabs(s, x..end, tab_y, active);
            if marker {
                s.put(end, 0, 1, "*", UNSAVED);
            }
            self.visible.tab_y = tab_y;
        }
        let rows = self.rows();
        let narrow = s.w < NARROW;
        let narrow_editor = narrow && tab.is_some() && !rows.is_empty();
        let preferences = matches!(self.panel, Panel::Appearance | Panel::Workflow);
        if !narrow_editor && self.panel != Panel::Exit && end.saturating_sub(y) > rows.len().clamp(1, 3) {
            // In preference lists, keep the blank rows only if there is also room for selected help.
            let spare = end.saturating_sub(y + rows.len());
            let padding = usize::from(end >= 12 && (!preferences || spare >= 6));
            y += padding;
            self.title(s, y);
            y += 1 + padding;
        }
        if self.panel == Panel::Diagnostics {
            self.draw_diagnostics(s, y, end);
            return;
        }

        let width = s.w.saturating_sub(2);
        let selected = rows.get(self.selected).map(|&row| (row, self.cell(row)));
        let detail_width = s.w - 1;
        let summary = selected
            .filter(|&(_, cell)| narrow && (cell.value.is_some() || cell.label != cell.short))
            .map(|(_, cell)| compact_detail(cell, detail_width));
        // Reserve space only for a complete summary and leave at least one control on screen.
        let space = end.saturating_sub(y);
        let summary_rows = summary.as_ref().map_or(0, |lines| lines.clone().take(space).count());
        let summary_rows = if summary_rows < space { summary_rows } else { 0 };
        if let Some(text) = self.intro() {
            let spare = end.saturating_sub(y + rows.len().min(3) + summary_rows);
            let review = if self.panel == Panel::Confirm(Confirm::MirrorAlerts) {
                self.alert_summary.as_ref().and_then(|summary| summary.settings.forwarding_review())
            } else {
                None
            };
            let review = review.iter().flat_map(|text| wrap(text, width));
            for line in wrap(text, width).chain(review).take(spare) {
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
        }

        if let (Some(Rect { x, y, width, height }), Some(preview)) = (diagram, preview)
            && y < end
        {
            preview.caption(s, x, y, width, !self.installed || self.layout_dirty());
            let highlight = match rows.get(self.selected) {
                Some(Row::Pane(pane)) => Some(*pane),
                Some(Row::Order(at)) => Some(self.layout.geometry.order[*at]),
                _ => None,
            };
            preview.draw(s, Rect { x, y: y + 1, width, height }, highlight);
        }
        let (rows_end, content_end) =
            if let Some(lines) = summary.filter(|_| summary_rows > 0 && summary_rows < form_end.saturating_sub(y)) {
                let top = (y + rows.len()).min(form_end - summary_rows);
                for (i, (line, value)) in lines.enumerate() {
                    let at = top + i;
                    let used = s.put(1, at, detail_width, line, FAINT);
                    if let Some(value) = value {
                        s.put(1 + used, at, 1, ":", FAINT);
                        if !value.is_empty() {
                            s.put(used + 3, at, detail_width - used - 2, value, FAINT);
                        }
                    }
                }
                (top, top + summary_rows)
            } else {
                let next = (y + rows.len()).min(form_end);
                let detail_space = if preferences {
                    // Keep several choices in view while reserving room for selected help.
                    form_end.saturating_sub(y + rows.len().min(3))
                } else {
                    form_end.saturating_sub(next + 1)
                };
                let mut rows_end = form_end;
                let mut content_end = next;
                if !narrow && detail_space > 0 && let Some((row, cell)) = selected {
                    let lines = wrap(self.detail(row, cell), form_width - 3).take(detail_space);
                    let count = lines.clone().count();
                    if count > 0 {
                        let gap = usize::from(next + count < form_end);
                        rows_end = next.min(form_end - count - gap);
                        let top = rows_end + gap;
                        for (i, line) in lines.enumerate() {
                            s.put(2, top + i, form_width - 3, line, FAINT);
                        }
                        content_end = top + count;
                    }
                }
                (rows_end, content_end)
            };
        self.draw_rows(s, y, rows_end, form_width);

        // A sample is decoration: let the controls and selected help use the space first.
        let sample_height = 4;
        if end.saturating_sub(content_end) >= sample_height {
            let top = end - sample_height;
            match self.panel {
                Panel::Appearance => self.appearance_sample(s, top),
                Panel::Workflow => self.workflow_sample(s, top),
                _ => {}
            }
        }
    }

    fn heading(&self, s: &mut Screen, dirty: bool) {
        if dirty || self.panel == Panel::Exit {
            let label = if s.w >= 16 { Panel::Exit.title() } else { "Unsaved" };
            s.put_fit(1, 0, s.w - 1, label, UNSAVED);
        } else {
            ui::section_heading(s, 0, s.w - 1, "Milo setup");
        }
    }

    fn notice(&self, s: &mut Screen) -> usize {
        let end = s.h - 1;
        let Some((text, style)) = self.message.as_ref() else { return end };
        let width = s.w.saturating_sub(2).max(1);
        // Leave room for both choices when a successful save opened the next scope prompt.
        let minimum = if self.panel.modal() && *style == FAINT { 3 } else { 2 };
        let lines = wrap(text, width).take((s.h / 3).min(end.saturating_sub(minimum)));
        let top = end - lines.clone().count();
        for (i, line) in lines.enumerate() {
            s.put(1, top + i, width, line, *style);
        }
        top
    }

    fn title(&self, s: &mut Screen, y: usize) {
        let show_heading = !matches!(self.panel, Panel::Layout | Panel::Appearance | Panel::Workflow) || !self.inside;
        let at = if show_heading {
            let heading = if s.w < NARROW {
                match self.panel {
                    Panel::Save(SaveKind::Layout) => "Save layout",
                    Panel::Save(_) => "Save prefs",
                    Panel::Confirm(Confirm::UseGlobal | Confirm::UseGlobalPrefs(_)) => "Use global?",
                    Panel::Confirm(Confirm::Repair) => "Repair?",
                    Panel::Confirm(Confirm::Remove) => "Remove?",
                    Panel::Confirm(Confirm::RemoveMenuBar) => "Remove app?",
                    Panel::Confirm(Confirm::MirrorAlerts) => "Mirror?",
                    Panel::Confirm(Confirm::Presence) => "Discord?",
                    Panel::Confirm(Confirm::PresenceDetails(_)) => "Show names?",
                    Panel::Confirm(Confirm::PresenceApp) => "Your app?",
                    _ => self.panel.title(),
                }
            } else {
                self.panel.title()
            };
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
            Panel::Appearance | Panel::Workflow => match self.prefs_scope() {
                Source::Default => "Built-in defaults",
                Source::Global => "Global preferences",
                Source::Worktree => "Worktree preferences",
            },
            Panel::Alerts => match &self.alert_summary {
                None => "Alert settings unreadable",
                Some(summary) if !summary.settings.enabled => "Off",
                Some(summary) if summary.running => "On",
                Some(_) => "On, but not running",
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
                || matches!(self.panel, Panel::Appearance | Panel::Workflow) && self.prefs_scope() == Source::Worktree;
            if scoped_to_worktree && let Some(name) = self.worktree_name() {
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
            Panel::Tune | Panel::Presence => return None,
            Panel::Appearance => return None,
            Panel::Workflow => return None,
            Panel::Alerts => "Rex's own notifications stay as they are. Each choice here is saved as you make it.",
            Panel::Health => "Inspect settings without changing them. Repair or remove only Milo's integration.",
            Panel::Diagnostics => return None,
            Panel::Exit => "Save before leaving?",
            Panel::Save(SaveKind::Layout) if self.only_auto_arrange_dirty() => {
                "Auto-arrange needs a global save."
            }
            Panel::Save(SaveKind::Layout) => {
                "On save: install or update Milo's Rex actions. Auto-arrange changes only with a global save."
            }
            Panel::Save(_) => "Save these appearance and workflow choices at the selected scope.",
            Panel::Confirm(Confirm::UseGlobal) => {
                "Remove this worktree's saved layout override and discard its pane arrangement draft."
            }
            Panel::Confirm(Confirm::UseGlobalPrefs(_)) => {
                "Remove this worktree's appearance and workflow override and discard the preference draft."
            }
            Panel::Confirm(Confirm::Repair) => {
                "Repair Milo's saved Rex integration without changing the layout choices. Rex may reload."
            }
            Panel::Confirm(Confirm::Remove) => {
                "Remove integration Milo owns from Rex and Claude Code, and end alert mirroring with its saved destinations. Open panes and personal preferences are left alone."
            }
            Panel::Confirm(Confirm::RemoveMenuBar) => {
                "Remove only the menu bar app. The Milo CLI and pane settings stay as they are."
            }
            Panel::Confirm(Confirm::MirrorAlerts) => {
                match self.alert_summary.as_ref().map(|summary| summary.settings.content) {
                    Some(alerts::Content::Text) => {
                        "Each enabled destination receives the alert kind, the program's short name, and up to 300 characters the program gave Rex, which can name files or quote a prompt. Rex's own notifications are unchanged."
                    }
                    _ => {
                        "Each enabled destination receives the alert kind and the program's short name, such as “Rex: claude-code is waiting for permission”. Nothing the program wrote is sent. Rex's own notifications are unchanged."
                    }
                }
            }
            Panel::Confirm(Confirm::Presence) => self.presence_view.as_ref()?.setting.public(),
            Panel::Confirm(Confirm::PresenceDetails(details)) => {
                let own = self.presence_view.as_ref().is_some_and(|status| status.setting.application.is_some());
                crate::presence::public(details, own)
            }
            Panel::Confirm(Confirm::PresenceApp) => crate::presence::APPLICATION_HELP,
        })
    }

    fn footer(&self, s: &mut Screen) -> help::HintHits<'static> {
        let back = self.panel.modal()
            || matches!(self.panel, Panel::Tune | Panel::Alerts | Panel::Presence | Panel::Diagnostics);
        let exit = match (back, s.w) {
            (true, 26..) => "Esc back",
            (true, 18..=23) => "back",
            (true, 12..) => "Esc",
            (true, _) => "‹",
            (false, 18..=23) => "quit",
            (false, 15..) => "q quit",
            (false, _) => "q",
        };
        let full = s.w >= 54;
        let compact = s.w >= NARROW;
        let save = if full {
            "s save…"
        } else if compact {
            "s save"
        } else if s.w >= 18 {
            "save"
        } else {
            "s"
        };
        let (first, middle): (&'static str, &'static [&'static str]) = match self.panel {
            Panel::Layout | Panel::Tune if self.inside && self.saved.is_some() => {
                let middle: &'static [&'static str] = match (self.trial.is_some(), full, compact) {
                    (true, true, _) => &["u undo", "Tab sections", "? help"],
                    (false, true, _) => &["a try", "Tab sections", "? help"],
                    (true, false, true) => &["u undo", "?"],
                    (false, false, true) => &["a try", "?"],
                    (true, false, false) if s.w >= 18 => &["undo", "?"],
                    (false, false, false) if s.w >= 18 => &["try", "?"],
                    (true, false, false) => &["u", "?"],
                    (false, false, false) => &["a", "?"],
                };
                (save, middle)
            }
            Panel::Appearance | Panel::Workflow if self.saved_prefs.is_some() => {
                (save, if full { &["Tab sections", "? help"] } else { &["?"] })
            }
            Panel::Diagnostics if full => ("r refresh", &["↑ up", "↓ down", "? help"]),
            Panel::Diagnostics if compact => ("r refresh", &["↑", "↓", "?"]),
            Panel::Diagnostics => ("r", &["↑", "↓", "?"]),
            _ if full && self.panel.tab().is_some() => ("Tab sections", &["? help"]),
            _ => ("", if compact { &["? help"] } else { &["?"] }),
        };
        // The last middle item is always help; don't advertise it when it cannot open.
        let middle = if help::Page::fits(s) { middle } else { &middle[..middle.len() - 1] };
        if full { help::draw_hint(s, first, middle, exit) } else { help::draw_compact_hint(s, first, middle, exit) }
    }

    fn draw_rows(&mut self, s: &mut Screen, y: usize, end: usize, width: usize) {
        let rows = self.rows();
        let len = end.saturating_sub(y).min(rows.len());
        let first = ui::selection_start(self.selected, rows.len(), len);
        let has_gutter = width >= 4;
        let draw_width = width - usize::from(has_gutter);
        let left = match width {
            1 => 0,
            2..=7 => 1,
            _ => 2,
        };
        let narrow = width < NARROW;
        self.visible.first = first;
        self.visible.y = y;
        self.visible.len = len;
        for (i, &row) in rows[first..first + len].iter().enumerate() {
            let row_y = y + i;
            let cell = self.cell(row);
            let selected = first + i == self.selected;
            let disabled = self.disabled(row);
            let marker = selected && left != 0;
            if marker {
                s.put(0, row_y, 1, "›", if disabled { FAINT } else { ACCENT.bold() });
            }
            let name_style = if disabled {
                FAINT
            } else if selected {
                if marker { HEADING } else { HEADING.reverse() }
            } else {
                Style::PLAIN
            };
            let hit = if let Some((value, style)) = cell.value {
                let space = draw_width - left;
                let gap = usize::from(space > 3);
                let value = if narrow { cell.short_value.unwrap_or(value) } else { value };
                let room = if narrow {
                    let width = str_width(value);
                    if width + str_width(cell.short) + gap <= space { width } else { 0 }
                } else {
                    ((space - gap) / 2).clamp(1, 13)
                };
                let start = draw_width - room;
                let label_room = start - left - gap * usize::from(room > 0);
                let label = cell.row_label(narrow, label_room);
                let label_width = control_label(s, left, row_y, label_room, label, name_style);
                let value_style = if disabled { FAINT } else { style };
                let value_width = if room == 0 {
                    0
                } else if narrow {
                    s.put(start, row_y, room, value, value_style)
                } else {
                    s.put_fit(start, row_y, room, value, value_style)
                };
                RowHit { marker, label: left..left + label_width, value: start..start + value_width }
            } else {
                let room = draw_width - left;
                let used = control_label(s, left, row_y, room, cell.row_label(narrow, room), name_style);
                RowHit { marker, label: left..left + used, ..RowHit::default() }
            };
            self.visible.rows[i] = hit;
        }
        if has_gutter {
            ui::thumb(s, draw_width, y, len, rows.len(), first);
        }
    }

    fn draw_diagnostics(&mut self, s: &mut Screen, y: usize, end: usize) {
        let left = usize::from(s.w >= 3);
        let gutter = usize::from(s.w >= 4);
        let width = if gutter != 0 { s.w - 3 } else { s.w - left };
        let text = if self.diagnostics.checking() {
            super::doctor::CHECKING
        } else if self.health.is_empty() {
            "No diagnostics were returned."
        } else {
            self.health.as_str()
        };
        let lines = wrap(text, width);
        let len = lines.clone().count();
        let shown = end.saturating_sub(y);
        self.visible.scroll_limit = len.saturating_sub(shown);
        self.health_top = self.health_top.min(self.visible.scroll_limit);
        for (i, line) in lines.skip(self.health_top).take(shown).enumerate() {
            s.put(left, y + i, width, line, Style::PLAIN);
        }
        self.visible.len = shown;
        if gutter != 0 {
            ui::thumb(s, s.w - 1, y, shown, len, self.health_top);
        }
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
            Row::Presence | Row::PresenceOpen => match &self.presence_view {
                Some(status) if status.setting.enabled => &self.presence_note,
                Some(_) => cell.detail,
                None => "Could not read the saved Discord presence choice. Milo will leave it alone.",
            },
            Row::ClaudeLimits | Row::SaveClaude => match self.claude {
                Claude::Foreign => "Milo will leave your Claude Code status line alone.",
                Claude::NotOwned => "You already added Milo to this Claude Code status line.",
                Claude::Invalid => "Could not read Claude Code settings. Milo will leave them alone.",
                Claude::On | Claude::Off => cell.detail,
            },
            Row::AlertsOn => self
                .alert_summary
                .as_ref()
                .map(|summary| summary.note.as_str())
                .filter(|note| !note.is_empty())
                .unwrap_or(cell.detail),
            Row::Icons if self.icon_override.is_some() => "MILO_ICONS overrides this saved icon choice.",
            Row::FollowProject if !self.inside => "Outside Rex, keep the starting project open.",
            Row::SaveGlobal if self.panel == Panel::Save(SaveKind::Layout) && self.scope == Scope::Worktree => {
                "Updates the global default without changing this worktree's override."
            }
            Row::SaveGlobal
                if matches!(self.panel, Panel::Save(SaveKind::Appearance | SaveKind::Workflow))
                    && self.prefs_scope() == Source::Worktree =>
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
            Row::Sidebar => Cell::action("Project sidebar", "Sidebar", "Files, commits and usage beside your shell."),
            Row::Capture => Cell::action(
                "Use this tab’s layout",
                "Use tab layout",
                "Read Milo's side, order and split sizes without moving anything.",
            ),
            Row::Tune => Cell::action("Customize panes", "Customize panes", "Choose the panes, side, order and size."),
            Row::Undo => Cell::action("Undo trial", "Undo trial", "Close only the panes created by this trial."),
            Row::SaveLayout => {
                Cell::action("Save layout for…", "Save layout", "Choose a worktree override or the global default.")
            }
            Row::UseGlobal => {
                Cell::action("Use global default", "Use global", "Remove this worktree's layout override.")
            }
            Row::Pane(pane) => Cell::toggle(
                pane_label(pane),
                pane_label(pane),
                layout.chosen(pane),
                "Show or hide this pane in the draft.",
            ),
            Row::Side => Cell::choice(
                "Rail side",
                "Side",
                if layout.geometry.side == Side::Left { "Left" } else { "Right" },
                "Which side of your shell holds Milo panes.",
            ),
            Row::Width => Cell::choice(
                "Rail width",
                "Width",
                layout.width_name(),
                "Cycle relative widths. The preview shows the draft percentage.",
            ),
            Row::Order(index) => {
                let (label, short) =
                    [("First position", "First"), ("Second position", "Second"), ("Third position", "Third")][index];
                Cell::choice(
                    label,
                    short,
                    pane_label(layout.geometry.order[index]),
                    "Swap the role in this position. Hidden panes are skipped.",
                )
            }
            Row::OutlineShare => {
                let value =
                    if layout.geometry.shares.is_some() { "Captured" } else { fraction_label(&layout.outline_share) };
                Cell::choice(
                    "Outline share",
                    "Share",
                    value,
                    "Outline's share above or below History, excluding Usage. Changing this replaces captured heights.",
                )
                .with_short_value(if value.len() > 9 { "Custom" } else { value })
            }
            Row::UsageRows => {
                let value = if layout.geometry.shares.is_some() { "Captured" } else { &layout.usage_rows };
                Cell::choice(
                    "Usage rows",
                    "Rows",
                    value,
                    "Target rows for Usage, capped at 40% of a shared rail. Changing this replaces captured heights.",
                )
                .with_short_value(if value.len() > 10 { "Custom" } else { value })
            }
            Row::AutoArrange => Cell::toggle(
                "Arrange new shell tabs",
                "New tabs",
                layout.new_tabs,
                "Global only. New command tabs and tabs with a layout are skipped.",
            ),
            Row::Cancel if self.panel == Panel::Exit => {
                Cell::action("Keep editing", "Keep editing", "Return to Settings without saving.")
            }
            Row::SaveAndExit => Cell::action("Save", "Save", "Save changes and leave Settings."),
            Row::Discard => Cell::action("Discard", "Discard", "Leave Settings without saving these changes."),
            Row::Back | Row::Cancel => Cell::action("Back", "Back", "Return without writing settings."),
            Row::Icons => {
                let value = prefs.icons.label();
                Cell::choice("Icons", "Icons", value, "Choose Nerd Font symbols, plain symbols, or no icons.")
                    .with_short_value(if prefs.icons == super::IconMode::Nerd { "Nerd" } else { value })
            }
            Row::Activity => Cell::choice(
                "Activity marks",
                "Motion",
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
                "Usage",
                if prefs.usage_large_figures { "Large figures" } else { "Auto compact" },
                "Auto uses text in narrow panes and large figures where there is room.",
            )
            .with_short_value(if prefs.usage_large_figures { "Large" } else { "Auto" }),
            Row::UsageTool(tool) => {
                use crate::preferences::{Show, UsageTool};
                let (label, short) = match tool {
                    UsageTool::Codex => ("Codex usage", "Codex"),
                    UsageTool::Claude => ("Claude Code usage", "Claude"),
                    UsageTool::OpenCode => ("OpenCode usage", "OpenCode"),
                };
                let show = prefs.usage_tools[tool];
                Cell::choice(
                    label,
                    short,
                    show.label(),
                    "Auto shows the tool once it has a figure saved here. Always show keeps its place and says what is unknown. Hide never looks for it.",
                )
                .with_short_value(if show == Show::Always { "Always" } else { show.label() })
            }
            Row::FollowProject => Cell::choice(
                "Project tracking",
                "Project",
                prefs.project_mode.label(),
                "Follow the originating shell, or stay with the starting project.",
            )
            .with_short_value(if prefs.project_mode == super::ProjectMode::Pin { "Pin" } else { "Follow" }),
            Row::FollowActivity => Cell::toggle(
                "Follow file edits",
                "Track edits",
                prefs.follow_activity,
                "Reveal active files without remembering temporary reveals.",
            ),
            Row::ChangesOnly => Cell::choice(
                "Outline starts with",
                "Outline",
                if prefs.changes_only { "Changes" } else { "All files" },
                "Set the initial Outline view. Press c in Outline to switch.",
            )
            .with_short_value(if prefs.changes_only { "Changes" } else { "All" }),
            Row::HistoryFollow => Cell::toggle(
                "Follow new commits",
                "New commits",
                prefs.history_follow_new,
                "Keep History at the newest commit while you are at the top.",
            ),
            Row::TabTitles => Cell::toggle(
                "Title Rex tabs",
                "Tab titles",
                prefs.tab_titles,
                "Name unnamed tabs for their Git project, worktree and agent.",
            ),
            Row::PreviewPlacement => Cell::choice(
                "Preview placement",
                "Preview",
                prefs.preview_placement.label(),
                "Choose where Preview opens relative to the working terminal.",
            ),
            Row::ClaudeLimits => {
                if self.claude.editable() {
                    Cell::toggle(
                        "Show Claude Code limits",
                        "Claude",
                        self.show_limits,
                        "Use Milo's Claude Code status line to read limits.",
                    )
                } else {
                    Cell::choice("Show Claude Code limits", "Claude", "Unchanged", "").with_short_value("Locked")
                }
            }
            Row::SavePrefs => {
                Cell::action("Save pane preferences for…", "Save prefs", "Save the appearance and workflow draft.")
            }
            Row::UseGlobalPrefs => Cell::action(
                "Use global pane preferences",
                "Use global",
                "Remove this worktree's pane preferences override.",
            ),
            Row::Alerts => Cell::action(
                "Mirror Rex alerts…",
                "Rex alerts",
                "Send chosen Rex alerts to Telegram, Discord, or both. Off until you turn it on.",
            ),
            Row::AlertsOn => Cell::toggle(
                "Mirror Rex alerts",
                "Mirror",
                self.alert_summary.as_ref().is_some_and(|summary| summary.settings.enabled),
                "Send the chosen alerts to enabled destinations. Rex's own notifications stay as they are.",
            ),
            Row::AlertsDestination => Cell::choice(
                "Destination",
                "To",
                match self.alert_summary.as_ref().and_then(|summary| summary.settings.destination) {
                    Some(destination) => destination.title(),
                    None => "None saved",
                },
                "Choose a saved destination for tests. To enable Telegram and Discord independently, open Alert forwarding in milo web.",
            ),
            Row::AlertKind(kind) => {
                let (label, short, detail) = match kind {
                    alerts::Kind::Blocked => (
                        "Waiting for you",
                        "Blocked",
                        "A program says it is blocked on a permission, a question or a sign-in.",
                    ),
                    alerts::Kind::Error => ("Reported an error", "Error", "A program says it failed."),
                    alerts::Kind::Done => ("Finished", "Done", "A program says it finished."),
                    alerts::Kind::Notification => (
                        "Desktop notification",
                        "Notify",
                        "A program asks for a desktop notification. A repeat within 30 seconds is not sent.",
                    ),
                    alerts::Kind::Bell => {
                        ("Terminal bell", "Bell", "A program rings the bell. At most one per terminal in 30 seconds.")
                    }
                };
                let on = self.alert_summary.as_ref().is_some_and(|summary| summary.settings.mirrors(kind));
                Cell::toggle(label, short, on, detail)
            }
            Row::AlertsContent => {
                let text = self
                    .alert_summary
                    .as_ref()
                    .is_some_and(|summary| summary.settings.content == alerts::Content::Text);
                Cell::choice(
                    "What is sent",
                    "Sends",
                    if text { "Kind and text" } else { "Kind only" },
                    "Kind only sends the kind of alert and the program's short name, and nothing the program wrote. Kind and text adds up to 300 characters the program gave Rex, which can name files or quote a prompt.",
                )
                .with_short_value(if text { "Text" } else { "Kind" })
            }
            Row::AlertsTest => Cell::action(
                "Send a test message",
                "Send test",
                "Send one message written by Milo to the destination. No Rex alert is involved.",
            ),
            Row::MenuBar => match self.menu_installed {
                Some(false) => Cell::action(
                    "Install Milo menu bar app",
                    "Install app",
                    "Install and open the separate macOS menu bar app.",
                ),
                Some(true) => Cell::action(
                    "Open or update Milo menu bar app",
                    "Open/update app",
                    "Open the menu bar app, updating it if the bundled copy differs.",
                ),
                None => unreachable!("the menu bar action is hidden when unavailable"),
            },
            Row::MenuIndicator => Cell::choice(
                "Menu bar indicator",
                "Indicator",
                self.menu_indicator.map_or("Unavailable", crate::menubar::Indicator::label),
                if self.menu_indicator.is_some() {
                    "Enter cycles Agent quota, CPU and Memory. Saves immediately for this profile without installing the app."
                } else {
                    "Could not read the saved indicator. Enter to reset it to Agent quota."
                },
            ),
            Row::PresenceOpen => Cell::toggle(
                "Discord presence…",
                "Discord",
                self.presence_view.as_ref().is_some_and(|status| status.setting.enabled),
                "Optionally show “In a Rex session” on your Discord profile. Off until you turn it on. Enter opens its choices.",
            ),
            Row::Presence => Cell::toggle(
                "Discord presence",
                "Discord",
                self.presence_view.as_ref().is_some_and(|status| status.setting.enabled),
                "Show “In a Rex session” on your Discord profile while Rex is in use. Off until you turn it on, and it asks first.",
            ),
            Row::PresenceDetails => {
                use crate::presence::Details;
                let details = self.presence_view.as_ref().map_or(Details::Generic, |status| status.setting.details);
                Cell::choice(
                    "Presence shows",
                    "Shows",
                    details.label(),
                    match details {
                        Details::Generic => "Only the fixed line: no session, project or other names. Enter moves on to the session name.",
                        Details::Session => "Also the name of the Rex session in front. Enter moves on to the project name instead.",
                        Details::Project => "Also the folder name of the Git project in the focused pane of the session in front. Enter moves on to both names.",
                        Details::Both => "Also the session's name and the project's folder name. Enter goes back to no names.",
                    },
                )
                .with_short_value(match details {
                    Details::Generic => "Generic",
                    Details::Session => "Session",
                    Details::Project => "Project",
                    Details::Both => "Both",
                })
            }
            Row::PresenceApp => {
                if self.presence_view.as_ref().is_some_and(|status| status.setting.application.is_some()) {
                    Cell::choice(
                        "Presence application",
                        "App",
                        "Your own",
                        "Shown under the name and icon of your own Discord application. Enter goes back to Milo's.",
                    )
                    .with_short_value("Own")
                } else {
                    Cell::choice(
                        "Presence application",
                        "App",
                        "Milo",
                        "Shown under Milo's name and icon. Enter asks for the ID of a Discord application of your own to use instead.",
                    )
                }
            }
            Row::PresenceId => Cell::choice(
                "Application ID",
                "App ID",
                if crate::presence::application_id(&self.application_draft).is_some() {
                    "Ready"
                } else if self.application_draft.is_empty() {
                    "Needed"
                } else {
                    "Typing"
                },
                "The 17 to 19 digit ID of a Discord application you created. Its name and icon are what Discord shows. Type the digits. Enter here clears them.",
            ),
            Row::RemoveMenuBar => Cell::action(
                "Remove Milo menu bar app…",
                "Remove app…",
                "Remove only the menu bar app, not the Milo CLI or pane settings.",
            ),
            Row::SaveClaude => Cell::action(
                "Save Claude Code choices",
                "Save Claude",
                "Only changes Claude Code's status line, if Milo owns it or it is unused.",
            ),
            Row::Diagnostics => Cell::action(
                "Read diagnostics",
                "Diagnostics",
                "Check preference scope, Rex, auto-arrange and usage data sources.",
            ),
            Row::Repair => Cell::action(
                "Repair integration…",
                "Repair",
                "Update Milo's Rex launcher and restart auto-arrange if needed.",
            ),
            Row::Remove => Cell::action(
                "Remove Milo integration…",
                "Remove",
                "Remove only the integrations Milo manages. Nothing is removed until you confirm.",
            ),
            Row::SaveGlobal => Cell::action(
                "As global default",
                "Global default",
                "Save for tabs and panes without a worktree override.",
            ),
            Row::SaveWorktree => {
                Cell::action("For this worktree", "This worktree", "Save only for the current Git worktree.")
            }
            Row::Continue => match self.panel {
                Panel::Confirm(Confirm::UseGlobal | Confirm::UseGlobalPrefs(_)) => {
                    Cell::action("Remove override", "Remove override", "Use the global setting from now on.")
                }
                Panel::Confirm(Confirm::Repair) => {
                    Cell::action("Repair integration", "Repair now", "Apply the repair now.")
                }
                Panel::Confirm(Confirm::Remove) => {
                    Cell::action("Remove integration", "Remove now", "Apply removal now.")
                }
                Panel::Confirm(Confirm::RemoveMenuBar) => {
                    Cell::action("Remove app now", "Remove app now", "Remove Milo from the menu bar.")
                }
                Panel::Confirm(Confirm::MirrorAlerts) => {
                    Cell::action("Turn mirroring on", "Turn on", "Start sending the chosen alerts now.")
                }
                Panel::Confirm(Confirm::Presence) => Cell::action(
                    "Show it on my Discord profile",
                    "Show on Discord",
                    "Saves the choice and starts showing it while Rex is in use.",
                ),
                Panel::Confirm(Confirm::PresenceDetails(_)) => Cell::action(
                    "Show these names on Discord",
                    "Show names",
                    "Saves the choice. Discord shows the names from now on.",
                ),
                Panel::Confirm(Confirm::PresenceApp) => Cell::action(
                    "Use this application",
                    "Use this app",
                    "Saves the ID. Discord shows your application's name and icon in place of Milo's.",
                ),
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
            Self::Workflow | Self::Alerts | Self::Presence => Some(2),
            Self::Health | Self::Diagnostics => Some(3),
            Self::Save(_) | Self::Confirm(_) | Self::Exit => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Layout => "Layout",
            Self::Tune => "Customize panes",
            Self::Presence => "Discord presence",
            Self::Appearance => "Appearance",
            Self::Workflow => "Workflow",
            Self::Alerts => "Mirror Rex alerts",
            Self::Health => "Health",
            Self::Diagnostics => "Diagnostics",
            Self::Save(SaveKind::Layout) => "Save layout for…",
            Self::Save(_) => "Save preferences for…",
            Self::Confirm(Confirm::UseGlobal | Confirm::UseGlobalPrefs(_)) => "Use global default?",
            Self::Confirm(Confirm::Repair) => "Repair integration?",
            Self::Confirm(Confirm::Remove) => "Remove integration?",
            Self::Confirm(Confirm::RemoveMenuBar) => "Remove menu app?",
            Self::Confirm(Confirm::MirrorAlerts) => "Mirror Rex alerts?",
            Self::Confirm(Confirm::Presence) => "Turn on Discord presence?",
            Self::Confirm(Confirm::PresenceDetails(_)) => "Show these names on Discord?",
            Self::Confirm(Confirm::PresenceApp) => "Use your own Discord application?",
            Self::Exit => "Unsaved changes",
        }
    }
}

fn control_label(s: &mut Screen, x: usize, y: usize, width: usize, text: &str, style: Style) -> usize {
    if width == 1 { s.put(x, y, width, text, style) } else { s.put_fit(x, y, width, text, style) }
}

#[derive(Clone, Copy)]
struct Cell<'a> {
    label: &'static str,
    short: &'static str,
    value: Option<(&'a str, Style)>,
    short_value: Option<&'a str>,
    detail: &'static str,
}

impl<'a> Cell<'a> {
    fn action(label: &'static str, short: &'static str, detail: &'static str) -> Self {
        Self { label, short, value: None, short_value: None, detail }
    }

    fn choice(label: &'static str, short: &'static str, value: &'a str, detail: &'static str) -> Self {
        Self { label, short, value: Some((value, ACCENT)), short_value: None, detail }
    }

    fn toggle(label: &'static str, short: &'static str, on: bool, detail: &'static str) -> Self {
        Self { label, short, value: Some(if on { ("On", ADD) } else { ("Off", FAINT) }), short_value: None, detail }
    }

    fn with_short_value(mut self, value: &'a str) -> Self {
        self.short_value = Some(value);
        self
    }

    fn row_label(self, narrow: bool, room: usize) -> &'static str {
        if !narrow && str_width(self.label) <= room { self.label } else { self.short }
    }
}

// Wrap borrowed strings separately so drawing a selection needs no formatted copy.
fn compact_detail(cell: Cell<'_>, width: usize) -> impl Iterator<Item = (&str, Option<&str>)> + Clone {
    let mut label = wrap(cell.label, width).peekable();
    let mut value = cell.value.map(|(value, _)| value);
    let mut remaining = wrap("", width);
    std::iter::from_fn(move || {
        let Some(line) = label.next() else {
            return remaining.next().map(|line| (line, None));
        };
        let suffix = if label.peek().is_none()
            && let Some(value) = value.take()
        {
            let used = str_width(line);
            if used + 2 + str_width(value) <= width {
                Some(value)
            } else {
                remaining = wrap(value, width);
                (used < width).then_some("")
            }
        } else {
            None
        };
        Some((line, suffix))
    })
}

fn tabs(s: &mut Screen, columns: Range<usize>, y: usize, active: usize) -> [Range<usize>; SECTIONS.len()] {
    let mut x = columns.start;
    let (labels, gap) = match columns.len() {
        36.. => (["Layout", "Appearance", "Workflow", "Health"], 2),
        24.. => (["Layout", "Looks", "Flow", "Health"], 1),
        17.. => (["Lay", "App", "Flow", "Hlth"], 1),
        7.. => (["L", "A", "W", "H"], 1),
        _ => (["L", "A", "W", "H"], 0),
    };
    std::array::from_fn(|index| {
        let text = labels[index];
        let style = if index == active { HEADING.reverse() } else { FAINT };
        let start = x;
        let used = s.put(x, y, text.len(), text, style);
        x += used + gap;
        start..start + used
    })
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
pub(crate) fn wrap(text: &str, width: usize) -> impl Iterator<Item = &str> + Clone {
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
