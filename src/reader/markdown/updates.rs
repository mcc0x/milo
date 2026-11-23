//! Match rendered blocks across saves without retaining either source file.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use super::{Document, Position, Row};
use crate::screen::{Screen, Style};

const LOOKAHEAD: usize = 16;
const MARK_TIME: Duration = Duration::from_secs(3);

/// Increasing pairs of old and new block indices. Text and layout identify a
/// block for navigation; marks also compare its rendered styles.
pub(super) struct Matches(Vec<(usize, usize)>);

impl Matches {
    pub(super) fn between(old: &Document, new: &Document) -> Self {
        let (n, m) = (old.blocks.len(), new.blocks.len());
        let common = n.min(m);
        let prefix = (0..common).take_while(|&i| old.key(i) == new.key(i)).count();
        let suffix = (1..=common - prefix).take_while(|&i| old.key(n - i) == new.key(m - i)).count();
        let mut pairs = Vec::with_capacity(prefix + suffix);
        pairs.extend((0..prefix).map(|i| (i, i)));
        if prefix < n - suffix && prefix < m - suffix {
            match_middle(old, new, prefix, n - suffix, m - suffix, &mut pairs);
        }
        pairs.extend((n - suffix..n).zip(m - suffix..m));
        Self(pairs)
    }

    pub(super) fn translate(&self, position: Position, old: &Document, new: &Document) -> Position {
        let Position { block, offset, limit } = position;
        let (block, offset) = match self.0.binary_search_by_key(&block, |pair| pair.0) {
            Ok(at) => (self.0[at].1, offset),
            Err(at) => {
                // An exact passage can survive a move outside the in-order diff.
                if let Some(moved) = moved_block(old, block, new) {
                    return Position { block: moved, offset, limit };
                }
                let (old_start, new_start) = at.checked_sub(1).map_or((0, 0), |i| {
                    let (old, new) = self.0[i];
                    (old + 1, new + 1)
                });
                let new_end = self.0.get(at).map_or(new.blocks.len(), |pair| pair.1);
                if let Some(position) = source_anchor(old, block, offset, new, new_start..new_end) {
                    position
                } else if new_start < new_end {
                    let new_block = (new_start + block - old_start).min(new_end - 1);
                    (new_block, translate_offset(old.key(block).1, new.key(new_block).1, offset))
                } else {
                    // The block was deleted; continue at the next surviving block,
                    // or the last one if there is no next block.
                    (new_start.min(new.blocks.len() - 1), 0)
                }
            }
        };
        Position { block, offset, limit }
    }
}

fn moved_block(old: &Document, block: usize, new: &Document) -> Option<usize> {
    let key = old.key(block);
    let mut found = (0..new.blocks.len()).filter(|&i| new.key(i) == key);
    let at = found.next()?;
    (found.next().is_none() && !(0..old.blocks.len()).any(|i| i != block && old.key(i) == key)).then_some(at)
}

/// Blank lines can split or merge paragraphs without changing the source line
/// being read. Keep the byte within that line if it survives once in the gap.
fn source_anchor(
    old: &Document,
    block: usize,
    offset: usize,
    new: &Document,
    gap: Range<usize>,
) -> Option<(usize, usize)> {
    if gap.is_empty() {
        return None;
    }
    let byte = old.blocks[block].start + offset;
    let at = old.sources.partition_point(|source| source.position <= (block, byte)) - 1;
    let within = byte - old.sources[at].position.1;
    let text = source_text(old, at);
    if within >= text.len() {
        return None;
    }
    let start = new.sources.partition_point(|source| source.position.0 < gap.start);
    let end = new.sources.partition_point(|source| source.position.0 < gap.end);
    let mut found = (start..end).filter(|&at| source_text(new, at) == text);
    let (block, byte) = new.sources[found.next()?].position;
    found.next().is_none().then_some((block, byte - new.blocks[block].start + within))
}

fn source_text(doc: &Document, at: usize) -> &str {
    let (block, start) = doc.sources[at].position;
    let end = match doc.sources.get(at + 1) {
        // A paragraph continuation is preceded by one synthetic space.
        Some(next) if next.position.0 == block => next.position.1 - 1,
        _ => doc.end(block),
    };
    &doc.text[start..end]
}

/// Keep a position in shared leading, trailing, or uniquely identifiable nearby text.
fn translate_offset(old: &str, new: &str, offset: usize) -> usize {
    let mut head = 0;
    for (a, b) in old.bytes().zip(new.bytes()) {
        if a != b {
            break;
        }
        head += 1;
        if head > offset {
            return offset;
        }
    }
    let remaining = old.len() - offset;
    let common_tail = old.len().min(new.len()) - head;
    if remaining != 0
        && remaining <= common_tail
        && old.bytes().rev().zip(new.bytes().rev()).take(remaining).all(|(a, b)| a == b)
    {
        return new.len() - remaining;
    }
    // A paragraph can have edits on both sides of the reading position.
    let end = old.floor_char_boundary((offset + 24).min(old.len()));
    if end - offset >= 8 {
        let needle = &old[offset..end];
        if let Some(at) = new.find(needle)
            && !new[new.ceil_char_boundary(at + 1)..].contains(needle)
        {
            return at;
        }
    }
    offset
}

#[derive(Clone, Copy)]
enum Occurrence {
    Missing,
    Once(usize),
    Repeated,
}

impl Occurrence {
    fn add(&mut self, at: usize) {
        *self = if matches!(self, Self::Missing) { Self::Once(at) } else { Self::Repeated };
    }
}

/// Intern block keys once so looking ahead never compares long text. Unique
/// blocks give distant anchors; bounded local matching fills the gaps between them.
fn match_middle(old: &Document, new: &Document, start: usize, n: usize, m: usize, pairs: &mut Vec<(usize, usize)>) {
    let mut keys = HashMap::new();
    let [old_ids, new_ids] = [(old, n, 0), (new, m, 1)].map(|(doc, end, side)| {
        (0..end - start)
            .map(|i| {
                let next = keys.len();
                let (id, occurrences) = keys.entry(doc.key(start + i)).or_insert((next, [Occurrence::Missing; 2]));
                occurrences[side].add(i);
                *id
            })
            .collect::<Vec<_>>()
    });
    let mut seen = vec![0; keys.len()];
    let mut candidates: Vec<_> = keys
        .into_values()
        .filter_map(|(_, seen)| match seen {
            [Occurrence::Once(old), Occurrence::Once(new)] => Some((old, new)),
            _ => None,
        })
        .collect();
    candidates.sort_unstable();
    let anchors = increasing(&candidates);
    let (mut a, mut b) = (0, 0);
    for (x, y) in anchors {
        match_gap(&old_ids[a..x], &new_ids[b..y], (a + start, b + start), &mut seen, pairs);
        pairs.push((x + start, y + start));
        (a, b) = (x + 1, y + 1);
    }
    match_gap(&old_ids[a..], &new_ids[b..], (a + start, b + start), &mut seen, pairs);
}

/// Longest in-order chain of the unique anchors, with one tail per chain length.
fn increasing(candidates: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = Vec::with_capacity(candidates.len());
    for (i, &(_, new)) in candidates.iter().enumerate() {
        let at = tails.partition_point(|&tail| candidates[tail].1 < new);
        previous.push(at.checked_sub(1).map(|p| tails[p]));
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut anchors = Vec::with_capacity(tails.len());
    let mut end = tails.last().copied();
    while let Some(i) = end {
        anchors.push(candidates[i]);
        end = previous[i];
    }
    anchors.reverse();
    anchors
}

fn match_gap(old: &[usize], new: &[usize], starts: (usize, usize), seen: &mut [u8], pairs: &mut Vec<(usize, usize)>) {
    if old.is_empty() || new.is_empty() {
        return;
    }
    const OLD: u8 = 1;
    const NEW: u8 = 2;
    for &id in old {
        seen[id] = OLD;
    }
    for &id in new {
        seen[id] |= NEW;
    }
    let (mut a, mut b) = (0, 0);
    while a < old.len() && b < new.len() {
        if old[a] == new[b] {
            pairs.push((starts.0 + a, starts.1 + b));
            (a, b) = (a + 1, b + 1);
        } else if seen[old[a]] == OLD {
            // A deletion or insertion with no counterpart need not spend
            // lookahead, even when the surrounding blocks all repeat.
            a += 1;
        } else if seen[new[b]] == NEW {
            b += 1;
        } else {
            let (removed, inserted) = nearby(&old[a..], &new[b..]).unwrap_or((1, 1));
            (a, b) = (a + removed, b + inserted);
        }
    }
    for &id in old.iter().chain(new) {
        seen[id] = 0;
    }
}

/// Try the fewest edits first, but do not search arbitrarily far through duplicates.
fn nearby(old: &[usize], new: &[usize]) -> Option<(usize, usize)> {
    for total in 1..=LOOKAHEAD {
        for a in total.saturating_sub(new.len() - 1)..=total.min(old.len() - 1) {
            if old[a] == new[total - a] {
                return Some((a, total - a));
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
struct Mark {
    block: usize,
    until: Instant,
}

#[derive(Default)]
pub(super) struct Marks {
    active: Vec<Mark>,
    deadline: Option<Instant>,
}

impl Marks {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| deadline <= now) {
            self.active.retain(|mark| mark.until > now);
            self.deadline = self.active.iter().map(|mark| mark.until).min();
        }
    }

    pub(super) fn update(&mut self, matches: &Matches, old: &Document, new: &Document, now: Instant) {
        let mut matched = matches.0.iter().copied().peekable();
        let mut marks = self.active.iter().copied().peekable();
        let mut active = Vec::new();
        let until = now + MARK_TIME;
        // The size-limit notice is not a document block.
        for block in 0..new.blocks.len() - usize::from(new.trimmed) {
            if let Some((prior, _)) = matched.next_if(|&(_, next)| next == block)
                && old.styles(prior).eq(new.styles(block))
            {
                while marks.next_if(|mark| mark.block < prior).is_some() {}
                if let Some(mark) = marks.next_if(|mark| mark.block == prior) {
                    active.push(Mark { block, ..mark });
                }
            } else {
                active.push(Mark { block, until });
            }
        }
        self.deadline = active.iter().map(|mark| mark.until).min();
        self.active = active;
    }

    pub(super) fn draw(&self, screen: &mut Screen, rows: &[Row]) {
        if screen.w < 2 {
            return; // There is no gutter; keep the document's only cell readable.
        }
        let Some(first) = rows.first() else { return };
        let from = self.active.partition_point(|mark| mark.block < first.block);
        let mut active = self.active[from..].iter().peekable();
        for (i, row) in rows.iter().enumerate() {
            while active.next_if(|mark| mark.block < row.block).is_some() {}
            let Some(mark) = active.peek() else { break };
            if mark.block == row.block {
                // Use the scroll gutter rather than rewrapping the document when a mark expires.
                screen.put(screen.w - 1, i + 1, 1, "!", Style::PLAIN.bold());
            }
        }
    }
}
