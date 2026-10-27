#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::Query;

fn find(text: &str, q: &str) -> Vec<(usize, usize)> {
    Query::new(q).unwrap().find(q, text)
}

#[test]
fn queries_match_as_section_3_1_says() {
    // ASCII case; whitespace runs for whitespace runs.
    assert_eq!(find("BOIL YOUR WATER", "boil your"), [(0, 9)]);
    assert_eq!(find("Boil\nwater now", "boil   water"), [(0, 10)]);
    assert_eq!(find("Boil \n water", "boil water"), [(0, 12)]);
    // Line breaks between characters are skipped, and stay in the range.
    assert_eq!(find("端午\n节放假", "端午节"), [(0, 10)]);
    assert_eq!(find("a\n\nb", "ab"), [(0, 4)]);
    // Left to right, never overlapping.
    assert_eq!(find("aaaa", "aa"), [(0, 2), (2, 4)]);
    // Nothing else is relaxed.
    assert_eq!(find("ＡＢ", "AB"), []);
    assert_eq!(find("Boilwater", "boil water"), []);
    // A match does not start, or end, on a skipped break.
    assert_eq!(find("x\n节", "节"), [(2, 5)]);
    assert_eq!(find("ab\n", "ab"), [(0, 2)]);
}

#[test]
fn whitespace_at_the_ends_of_a_query_counts() {
    assert_eq!(find("catapult cat ", "cat "), [(9, 13)]);
    assert_eq!(find("bobcat cat", " cat"), [(6, 10)]);
    assert_eq!(find("cat\n", "cat\n"), [(0, 4)]);
    // Only whitespace, or nothing, is no query.
    assert!(Query::new(" \n\t").is_none() && Query::new("").is_none());
}

#[test]
fn queries_past_64_items_and_multibyte_text_work() {
    let long = "通".repeat(200);
    let text = format!("xx{long}yy{long}");
    let hits = find(&text, &long);
    assert_eq!(hits, [(2, 602), (604, 1204)]);
    assert_eq!(&text[hits[1].0..hits[1].1], long);
}

/// `find`, and the steps it took.
fn counted(text: &str, q: &str) -> (Vec<(usize, usize)>, usize) {
    super::STEPS.with(|s| s.set(0));
    let hits = find(text, q);
    (hits, super::STEPS.with(std::cell::Cell::get))
}

/// The worst case of trying every start: a 16 KiB block of one letter and a
/// 500-character query that fails at its last character. One step a
/// character, where retrying from every start would take some 500 each.
/// Counted, not timed: a busy machine stretched a timed bound past 5 s for
/// the same work.
#[test]
fn the_worst_case_stays_linear() {
    let text = "a".repeat(16 * 1024);
    let query = format!("{}b", "a".repeat(499));
    assert_eq!(counted(&text, &query), (vec![], text.len()));
    // With matches, finding where each starts reads its own range back once:
    // at most two steps a character in all.
    let text = "a b ".repeat(8 * 1024);
    let (hits, steps) = counted(&text, "a b");
    assert_eq!(hits.len(), 8 * 1024);
    assert!(steps <= 2 * text.len(), "{steps} steps for {}", text.len());
}
