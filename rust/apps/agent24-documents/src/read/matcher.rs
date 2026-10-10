//! `find`'s matching (ADR-DOC-02 §3.1, Q14), in time linear in the text.
//!
//! A query is a chain of items: a character (ASCII letters in either case),
//! or a run of whitespace that matches a run of whitespace. Between two
//! characters the text may hold line breaks, which are skipped. That is an
//! automaton with self-loops, run bit-parallel (Shift-And, 64 states a
//! word): forwards to find where a match ends, then backwards from there to
//! find where it starts. Matches go left to right and never overlap.

use std::collections::HashMap;

/// A compiled query: for each state `k` (`k` items matched), what moves it
/// on to `k + 1` and what keeps it.
pub struct Query {
    items: usize,
    words: usize,
    /// Characters (ASCII-lowercased) an item takes.
    takes: HashMap<char, Vec<u64>>,
    /// Items that are a run of whitespace.
    space: Vec<u64>,
    /// States that a further whitespace keeps (the last item was a run).
    space_loop: Vec<u64>,
    /// States that a line break keeps (between two characters).
    break_loop: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Char(char),
    Space,
}

fn fold(c: char) -> char {
    c.to_ascii_lowercase()
}

fn set(bits: &mut [u64], k: usize) {
    bits[k / 64] |= 1 << (k % 64);
}

fn get(bits: &[u64], k: usize) -> bool {
    bits[k / 64] & (1 << (k % 64)) != 0
}

impl Query {
    /// `None` for a query that is only whitespace (or empty).
    #[must_use]
    pub fn new(query: &str) -> Option<Self> {
        Self::of(Self::items(query.chars()))
    }

    /// The same query read right to left, for finding where a match starts.
    fn reversed(query: &str) -> Option<Self> {
        Self::of(Self::items(query.chars().rev()))
    }

    fn items(chars: impl Iterator<Item = char>) -> Vec<Item> {
        let mut out = Vec::new();
        for c in chars {
            match (c.is_whitespace(), out.last()) {
                (true, Some(Item::Space)) => {}
                (true, _) => out.push(Item::Space),
                (false, _) => out.push(Item::Char(fold(c))),
            }
        }
        out
    }

    fn of(items: Vec<Item>) -> Option<Self> {
        if !items.iter().any(|i| matches!(i, Item::Char(_))) {
            return None;
        }
        let words = (items.len() + 1).div_ceil(64);
        let mut q = Query {
            items: items.len(),
            words,
            takes: HashMap::new(),
            space: vec![0; words],
            space_loop: vec![0; words],
            break_loop: vec![0; words],
        };
        for (k, item) in items.iter().enumerate() {
            match item {
                Item::Char(c) => set(q.takes.entry(*c).or_insert_with(|| vec![0; words]), k),
                Item::Space => {
                    set(&mut q.space, k);
                    set(&mut q.space_loop, k + 1);
                }
            }
            if k > 0 && matches!(item, Item::Char(_)) && matches!(items[k - 1], Item::Char(_)) {
                set(&mut q.break_loop, k);
            }
        }
        Some(q)
    }

    /// One step on `c`: `d` (the live states) becomes its successors.
    fn step(&self, d: &mut [u64], c: char) {
        let ws = c.is_whitespace();
        let takes = self.takes.get(&fold(c));
        let mut carry = 0;
        for w in 0..self.words {
            let mut moves = takes.map_or(0, |t| t[w]);
            let mut keeps = 0;
            if ws {
                moves |= self.space[w];
                keeps |= self.space_loop[w];
            }
            if c == '\n' {
                keeps |= self.break_loop[w];
            }
            let advanced = d[w] & moves;
            d[w] = (advanced << 1) | carry | (d[w] & keeps);
            carry = advanced >> 63;
        }
    }

    /// Byte ranges of the matches in `text`, left to right, not overlapping:
    /// each the first to end, from its earliest start.
    #[must_use]
    pub fn find(&self, query: &str, text: &str) -> Vec<(usize, usize)> {
        let Some(back) = Self::reversed(query) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut from = 0;
        let mut d = vec![0; self.words];
        for (i, c) in text.char_indices() {
            if i < from {
                continue;
            }
            // A match may start at any character from `from` on.
            d[0] |= 1;
            self.step(&mut d, c);
            if get(&d, self.items) {
                let end = i + c.len_utf8();
                let start = back.start(&text[from..end]).map_or(from, |s| from + s);
                out.push((start, end));
                (from, d) = (end, vec![0; self.words]);
            }
        }
        out
    }

    /// The earliest start of a match ending exactly at the end of `text`.
    fn start(&self, text: &str) -> Option<usize> {
        let mut d = vec![0; self.words];
        d[0] = 1;
        let mut earliest = None;
        for (i, c) in text.char_indices().rev() {
            self.step(&mut d, c);
            if get(&d, self.items) {
                earliest = Some(i);
            }
            if d.iter().all(|w| *w == 0) {
                break;
            }
        }
        earliest
    }
}

#[cfg(test)]
mod tests;
