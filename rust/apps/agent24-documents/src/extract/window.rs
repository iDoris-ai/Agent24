//! Windows of a text layer, and batches of fields (§3.2): what one model
//! call reads and answers. Each block counts as it is sent, its JSON with
//! id and page, so a window bounds the request, not just the text. A layer
//! that fits [`BUDGET`] is one window. A longer one is cut by pages, each
//! window carrying as much of the end of the page before as the room left
//! allows, so a condition and the proposition it governs across a page
//! break can be read together; a page over the budget is cut by blocks.

use serde_json::{Value, json};

use crate::text_layer::{Block, TextLayer};

/// One window's blocks, as sent, at most: with the instruction, 20 fields
/// and the answer's schema, a request stays far inside the kernel's 256 KiB.
pub const BUDGET: usize = 24 * 1024;
/// Fields in one call, at most.
pub const BATCH: usize = 20;

/// A window: indices into the layer's blocks, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window(pub Vec<usize>);

/// A block as the model is sent it.
#[must_use]
pub fn block_json(b: &Block) -> Value {
    json!({ "block_id": b.block_id, "page": b.page, "text": b.text })
}

/// What a block costs a window: its JSON and a comma.
fn cost(b: &Block) -> usize {
    block_json(b).to_string().len() + 1
}

impl Window {
    /// What it costs, as sent.
    #[must_use]
    pub fn bytes(&self, layer: &TextLayer) -> usize {
        self.0.iter().map(|&i| cost(&layer.blocks[i])).sum()
    }

    /// The window in two halves by blocks, for a model whose context it
    /// overflows; `None` for a single block, which cannot be cut.
    #[must_use]
    pub fn halves(&self) -> Option<(Window, Window)> {
        (self.0.len() > 1).then(|| {
            let (a, b) = self.0.split_at(self.0.len() / 2);
            (Window(a.to_vec()), Window(b.to_vec()))
        })
    }
}

/// The windows of `layer` within `budget` bytes each. A block is at most
/// 16 KiB of text (§3.1), so with its JSON it fits a window of 24 KiB
/// unless its text is escaped many times over.
#[must_use]
pub fn windows(layer: &TextLayer, budget: usize) -> Vec<Window> {
    let size = |i: usize| cost(&layer.blocks[i]);
    let all: Vec<usize> = (0..layer.blocks.len()).collect();
    if all.iter().map(|&i| size(i)).sum::<usize>() <= budget {
        return vec![Window(all)];
    }
    // Pieces in order: whole pages, or the parts of a page over the budget.
    let mut pieces: Vec<Vec<usize>> = Vec::new();
    for (i, b) in layer.blocks.iter().enumerate() {
        let same_page = pieces
            .last()
            .is_some_and(|p| layer.blocks[p[0]].page == b.page);
        match pieces.last_mut() {
            Some(p) if same_page => p.push(i),
            _ => pieces.push(vec![i]),
        }
    }
    let total = |blocks: &[usize]| blocks.iter().map(|&i| size(i)).sum::<usize>();
    let mut parts = Vec::new();
    for page in pieces {
        if total(&page) <= budget {
            parts.push(page);
            continue;
        }
        let mut part = Vec::new();
        for i in page {
            if !part.is_empty() && total(&part) + size(i) > budget {
                parts.push(std::mem::take(&mut part));
            }
            part.push(i);
        }
        parts.push(part);
    }
    let mut out = Vec::new();
    let mut next = 0;
    while next < parts.len() {
        let first = next;
        let mut blocks = Vec::new();
        // At least one part, so the windows always move on: a block whose
        // JSON alone is over the budget (text escaped many times over) goes
        // alone, and the kernel decides whether it is too large.
        while next < parts.len()
            && (blocks.is_empty() || total(&blocks) + total(&parts[next]) <= budget)
        {
            blocks.extend(&parts[next]);
            next += 1;
        }
        // A window starting a page carries the end of the page before, from
        // its last block back, as far as the room left allows.
        let start = blocks[0];
        let before = start.checked_sub(1).map(|i| layer.blocks[i].page);
        if let Some(page) = before.filter(|&p| first > 0 && p != layer.blocks[start].page) {
            let mut room = budget.saturating_sub(total(&blocks));
            let mut carried = Vec::new();
            for i in (0..start)
                .rev()
                .take_while(|&i| layer.blocks[i].page == page)
            {
                if size(i) > room {
                    break;
                }
                room -= size(i);
                carried.push(i);
            }
            carried.reverse();
            blocks.splice(0..0, carried);
        }
        out.push(Window(blocks));
    }
    out
}

/// Fields in batches of at most `size`, in order.
#[must_use]
pub fn batches<T: Clone>(fields: &[T], size: usize) -> Vec<Vec<T>> {
    fields.chunks(size.max(1)).map(<[T]>::to_vec).collect()
}

#[cfg(test)]
mod tests;
