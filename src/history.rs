//! The recent-history panel: a row of dots for the shape of it, then the
//! newest commits spelled out.
//!
//! Hollow is work not committed, yellow is committed but not pushed, green
//! is on the remote, and the bold dot is where the branch is now. Without a
//! remote, commits are simply uncoloured.

use std::collections::HashMap;
use std::time::Instant;

use crate::git::Commit;
use crate::term::{self, Screen, Style};
use crate::ui::DIM;
use crate::when;

/// From this many rows the panel leads with its row of dots.
const DOT_ROWS: usize = 6;

/// What the panel needs to know besides the commits themselves.
pub struct Context<'a> {
    /// What the branch is pushed to, by name.
    pub remote: Option<&'a str>,
    /// Files not committed yet.
    pub dirty: usize,
    /// Commits that landed while the outline was open, by hash.
    pub fresh: &'a HashMap<String, Instant>,
}

/// Length of a leading "scope: " in a commit subject: a short single word
/// and the colon and space after it. Zero if there is none.
fn prefix_len(subject: &str) -> usize {
    match subject.split_once(": ") {
        Some((scope, _)) if scope.len() <= 24 && !scope.contains(' ') => scope.len() + 2,
        _ => 0,
    }
}

fn dot_style(i: usize, c: &Commit, ctx: &Context) -> Style {
    let st = match (ctx.remote, c.pushed) {
        (None, _) => Style::PLAIN,
        (Some(_), true) => Style::fg(term::GREEN),
        (Some(_), false) => Style::fg(term::YELLOW),
    };
    // The branch tip is the same dot, bold.
    if i == 0 { st.bold() } else { st }
}

/// Whether history passes, between two neighbours, from local commits to
/// ones the remote has.
fn crosses(before: &Commit, c: &Commit, ctx: &Context) -> bool {
    ctx.remote.is_some() && !before.pushed && c.pushed
}

/// Rows the panel wants out of `avail` when it sits beneath a tree of
/// `tree_rows`: no more than it has to say, about a quarter of the pane,
/// more where the tree leaves room.
pub fn rows(commits: &[Commit], ctx: &Context, avail: usize, tree_rows: usize) -> usize {
    let unpushed = commits.iter().filter(|c| !c.pushed).count();
    let remote_row = (unpushed > 0 && unpushed < commits.len()) as usize;
    // A blank row above, the heading, the dots, then one row per line.
    let content = 3 + (ctx.dirty > 0) as usize + remote_row + commits.len();
    let spare = avail.saturating_sub(tree_rows).min(avail / 2);
    content.min((avail / 4).max(DOT_ROWS + 1).max(spare)).min(avail.saturating_sub(3))
}

/// The shape of recent history in one row: newest first, a gap between days,
/// a tick where the remote is.
fn draw_dots(s: &mut Screen, commits: &[Commit], ctx: &Context, (left, right): (usize, usize), y: usize) {
    // Spaced out while every commit fits that way, packed once they do not.
    let pairs = || commits.iter().zip(commits.iter().skip(1));
    let days = pairs().filter(|(a, b)| a.day != b.day).count();
    let ticks = pairs().filter(|(a, b)| crosses(a, b, ctx)).count();
    let cells = commits.len() + ticks + (ctx.dirty > 0) as usize;
    let step = if cells * 2 + days <= right.saturating_sub(left) { 2 } else { 1 };
    let mut x = left;
    if ctx.dirty > 0 {
        s.put(x, y, 1, "○", Style::PLAIN.bold());
        x += step;
    }
    for (i, c) in commits.iter().enumerate() {
        if let Some(before) = i.checked_sub(1).map(|b| &commits[b]) {
            if crosses(before, c, ctx) && x < right {
                s.put(x, y, 1, "│", DIM);
                x += step;
            }
            x += (before.day != c.day) as usize;
        }
        if x >= right {
            break;
        }
        s.put(x, y, 1, "●", dot_style(i, c, ctx));
        x += step;
    }
}

/// Draws the panel in the area whose corner is (x0, y0): a heading in the
/// project name's own weight, the dots where there is room for them, then
/// one commit a row.
pub fn draw(s: &mut Screen, commits: &[Commit], ctx: &Context, (x0, y0): (usize, usize), (w, rows): (usize, usize)) {
    if w < 4 || rows == 0 {
        return;
    }
    // Text keeps one column clear of both edges of the area.
    let (left, right, end) = (x0 + 1, x0 + w - 1, y0 + rows);
    let unpushed = commits.iter().filter(|c| !c.pushed).count();

    s.put(left, y0, w - 2, "History", Style::PLAIN.bold());
    // The project heading counts files to push; this counts commits, and says so.
    if unpushed > 0 {
        let text = format!("{unpushed} unpushed");
        s.put(right.saturating_sub(text.len()), y0, text.len(), &text, Style::fg(term::YELLOW));
    }
    // In a short pane every row goes to commits, one beneath the other.
    let dots = rows >= DOT_ROWS;
    if dots {
        draw_dots(s, commits, ctx, (left, right), y0 + 1);
    }

    let text_x = left + 2;
    let mut y = y0 + 1 + dots as usize;
    if ctx.dirty > 0 && y < end {
        let text = format!("{} file{} not committed", ctx.dirty, if ctx.dirty == 1 { "" } else { "s" });
        s.put(left, y, 1, "○", Style::PLAIN.bold());
        s.put_fit(text_x, y, right.saturating_sub(text_x), &text, Style::PLAIN.bold());
        y += 1;
    }
    let now = when::now();
    let mut last_age = String::new();
    for (i, c) in commits.iter().enumerate() {
        if i > 0 && crosses(&commits[i - 1], c, ctx) && y < end {
            // Everything below this name is already on the remote.
            s.put_fit(text_x, y, right.saturating_sub(text_x), ctx.remote.unwrap_or_default(), DIM);
            y += 1;
        }
        if y >= end {
            break;
        }
        s.put(left, y, 1, "●", dot_style(i, c, ctx));
        // An age is only worth its column where it differs from the row above.
        let age = when::short_age(now - c.time);
        let mut text_end = right;
        if age != last_age {
            text_end = text_end.saturating_sub(age.len());
            s.put(text_end, y, age.len(), &age, DIM);
            text_end = text_end.saturating_sub(1);
        }
        last_age = age;

        let room = text_end.saturating_sub(text_x);
        let fresh = ctx.fresh.contains_key(&c.hash);
        // Merges and what is already pushed recede; a commit that just
        // landed stands out for a moment.
        let quiet = c.merge || (c.pushed && ctx.remote.is_some());
        let st = match (fresh, quiet) {
            (true, _) => Style::PLAIN.bold(),
            (false, true) => Style::PLAIN.faint(),
            (false, false) => Style::PLAIN,
        };
        // A "scope: " prefix steps back so the eye lands on what follows.
        let split = if fresh || quiet { 0 } else { prefix_len(&c.subject) };
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
