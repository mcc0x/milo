//! Lays out recent commits and marks uncommitted work and the remote boundary.

use std::collections::HashMap;
use std::time::Instant;

use crate::git::Commit;
use crate::screen::{Screen, Style};
use crate::ui::FAINT;
use crate::when;

/// From this many rows a panel beside the outline leads with a row of dots.
const DOT_ROWS: usize = 6;
/// Lines of content below the heading in an ordinary pane with a full tree.
const FEW: usize = 3;
/// From this many body rows a panel beneath the tree asks for a quarter.
const TALL_ROWS: usize = 30;

/// What the panel needs to know besides the commits themselves.
pub struct Context<'a> {
    /// The name to show at the boundary with pushed commits.
    pub remote: Option<&'a str>,
    pub uncommitted: usize,
    /// Commits that landed while the outline was open, by hash.
    pub new_commits: &'a HashMap<String, Instant>,
}

/// The panel shares the outline's heading row beside it, or starts on its
/// own row beneath it. The panel beside the outline has room for dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Beside { left: usize, width: usize, rows: usize },
    Beneath { top: usize, width: usize, rows: usize },
}

/// A leading `scope: ` is dimmed so the rest of the subject stands out.
fn prefix_len(subject: &str) -> usize {
    match subject.split_once(": ") {
        Some((scope, _)) if scope.len() <= 24 && !scope.contains(' ') => scope.len() + 2,
        _ => 0,
    }
}

fn dot_style(c: &Commit, ctx: &Context) -> Style {
    match (ctx.remote, c.unpushed) {
        (None, _) => Style::PLAIN,
        (Some(_), false) => Style::PLAIN.faint(),
        (Some(_), true) => Style::PLAIN.bold(),
    }
}

/// Whether adjacent commits straddle the boundary with pushed history.
fn crosses_remote(before: &Commit, c: &Commit, ctx: &Context) -> bool {
    ctx.remote.is_some() && before.unpushed && !c.unpushed
}

/// Rows to set aside when history sits beneath a tree of `tree_rows`:
/// enough for its content, several on its own, more if the tree leaves room.
pub fn below_rows(commits: &[Commit], uncommitted: usize, avail: usize, tree_rows: usize) -> usize {
    let unpushed = commits.iter().filter(|c| c.unpushed).count();
    let remote_row = (unpushed > 0 && unpushed < commits.len()) as usize;
    // A blank row above and the heading, then one row per line.
    let content = 2 + usize::from(uncommitted > 0) + remote_row + commits.len();
    let spare = avail.saturating_sub(tree_rows).min(avail / 2);
    let share = if avail < TALL_ROWS { 2 + FEW } else { avail / 4 };
    content.min(share.max(spare)).min(avail.saturating_sub(3))
}

/// The shape of recent history in one row: newest first, a gap between days,
/// a tick where the remote is.
fn draw_dots(s: &mut Screen, commits: &[Commit], ctx: &Context, (left, right): (usize, usize), y: usize) {
    // Spaced out while every commit fits that way, packed once they do not.
    let pairs = || commits.iter().zip(commits.iter().skip(1));
    let days = pairs().filter(|(a, b)| a.day != b.day).count();
    let ticks = pairs().filter(|(a, b)| crosses_remote(a, b, ctx)).count();
    let cells = commits.len() + ticks + usize::from(ctx.uncommitted > 0);
    let step = if cells * 2 + days <= right.saturating_sub(left) { 2 } else { 1 };
    let mut x = left;
    if ctx.uncommitted > 0 {
        s.put(x, y, 1, "○", Style::PLAIN.bold());
        x += step;
    }
    for (i, c) in commits.iter().enumerate() {
        if let Some(before) = i.checked_sub(1).map(|b| &commits[b]) {
            if crosses_remote(before, c, ctx) && x < right {
                s.put(x, y, 1, "│", FAINT);
                x += step;
            }
            x += (before.day != c.day) as usize;
        }
        if x >= right {
            break;
        }
        s.put(x, y, 1, "●", dot_style(c, ctx));
        x += step;
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Draws the heading and recent commits in the panel's part of the pane.
pub fn draw(s: &mut Screen, commits: &[Commit], ctx: &Context, placement: Placement) {
    let (x0, y0, w, rows, dots) = match placement {
        Placement::Beside { left, width, rows } => (left, 0, width, rows, rows >= DOT_ROWS),
        Placement::Beneath { top, width, rows } => (0, top, width, rows, false),
    };
    if w < 4 || rows == 0 {
        return;
    }
    // Text keeps one column clear of both edges of the area.
    let (left, right, end) = (x0 + 1, x0 + w - 1, y0 + rows);
    let unpushed = commits.iter().filter(|c| c.unpushed).count();

    s.put(left, y0, w - 2, "History", Style::PLAIN.bold());
    // The project heading counts files; this counts commits, and says so.
    if unpushed > 0 && ctx.remote.is_some() {
        let text = format!("{unpushed} commit{} to push", plural(unpushed));
        if text.len() + 9 < w {
            s.put_right(right, y0, &text, Style::PLAIN);
        }
    }
    if dots {
        draw_dots(s, commits, ctx, (left, right), y0 + 1);
    }

    let text_x = left + 2;
    let mut y = y0 + 1 + dots as usize;
    if ctx.uncommitted > 0 && y < end {
        let text = format!("{} file{} to commit", ctx.uncommitted, plural(ctx.uncommitted));
        s.put(left, y, 1, "○", Style::PLAIN.bold());
        s.put_fit(text_x, y, right.saturating_sub(text_x), &text, Style::PLAIN.bold());
        y += 1;
    }
    let now = when::now();
    let mut last_age = String::new();
    for (i, c) in commits.iter().enumerate() {
        if i > 0 && crosses_remote(&commits[i - 1], c, ctx) && y < end {
            // Everything below this is already on the remote.
            let text = format!("pushed to {}", ctx.remote.unwrap_or_default());
            s.put_fit(text_x, y, right.saturating_sub(text_x), &text, FAINT);
            y += 1;
        }
        if y >= end {
            break;
        }
        s.put(left, y, 1, "●", dot_style(c, ctx));
        // An age is only worth its column where it differs from the row above.
        let age = when::short_age(now - c.time);
        let mut text_end = right;
        if age != last_age {
            let used = s.put_right(right, y, &age, FAINT);
            text_end = right.saturating_sub(used + 1);
        }
        last_age = age;

        let room = text_end.saturating_sub(text_x);
        let new = ctx.new_commits.contains_key(&c.hash);
        // Merges and what is already pushed recede; a commit that just
        // landed stands out for a moment.
        let quiet = c.merge || (!c.unpushed && ctx.remote.is_some());
        let st = match (new, quiet) {
            (true, _) => Style::PLAIN.bold(),
            (false, true) => Style::PLAIN.faint(),
            (false, false) => Style::PLAIN,
        };
        // A "scope: " prefix steps back so the eye lands on what follows.
        let split = if new || quiet { 0 } else { prefix_len(&c.subject) };
        let used = s.put(text_x, y, room, &c.subject[..split], Style::PLAIN.faint());
        s.put_fit(text_x + used, y, room.saturating_sub(used), &c.subject[split..], st);
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::prefix_len;

    #[test]
    fn a_scope_prefix_is_one_short_word() {
        assert_eq!(prefix_len("fix: off by one"), 5);
        assert_eq!(prefix_len("feat(ui): tabs"), 10);
        assert_eq!(prefix_len("Merge branch: not a scope"), 0);
        assert_eq!(prefix_len("No prefix here"), 0);
    }
}
