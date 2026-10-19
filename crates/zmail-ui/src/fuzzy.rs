//! A small fuzzy matcher for the command palette.
//!
//! A query matches a text when every word of the query appears in the text
//! as a subsequence: "arch" matches "Go to Archive", and "gt arc" does too.
//! Case is ignored. The score rewards what people mean when they type a
//! few letters: matches at the start of words ("Mark Read" for "mr"), runs
//! of consecutive letters, and a match at the very start of the text; gaps
//! between matched letters cost a little. Higher scores are better.
//!
//! Each word finds its best placement (a small dynamic programme over the
//! text, linear in its length per query letter), so "inbox" in "Go to
//! Unified Inbox" matches the word "Inbox" rather than letters scattered
//! through "Unified".

/// Score for each matched letter.
const MATCH: i32 = 16;
/// Extra score for a letter at the start of a word.
const WORD_START: i32 = 24;
/// Extra score for a letter right after the previous matched one.
const CONSECUTIVE: i32 = 20;
/// Extra score when the first letter of a word matches the text's first
/// letter.
const TEXT_START: i32 = 16;
/// Cost of each unmatched letter between two matched ones.
const GAP: i32 = 2;
/// Cost of each unmatched letter before a word's first match, capped.
const LEADING: i32 = 1;
const MAX_LEADING_PENALTY: i32 = 12;

/// A successful match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuzzyMatch {
    /// Higher is better.
    pub(crate) score: i32,
    /// Byte offsets into the text of the matched characters, ascending and
    /// without duplicates, for highlighting.
    pub(crate) positions: Vec<usize>,
}

/// Matches `query` against `text`. An empty or blank query matches
/// everything with a score of zero; otherwise every whitespace-separated
/// word of the query must match, and the scores add up.
pub(crate) fn fuzzy_match(query: &str, text: &str) -> Option<FuzzyMatch> {
    let haystack: Vec<(usize, char)> = text
        .char_indices()
        .map(|(offset, ch)| (offset, fold(ch)))
        .collect();
    let starts: Vec<bool> = word_starts(text);

    let mut score = 0;
    let mut positions = Vec::new();
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.chars().map(fold).collect();
        let (word_score, indices) = best_placement(&needle, &haystack, &starts)?;
        score += word_score;
        positions.extend(indices.into_iter().map(|index| haystack[index].0));
    }
    positions.sort_unstable();
    positions.dedup();
    Some(FuzzyMatch { score, positions })
}

/// Lowercases one character the simple way (the first character of its
/// lowercase form), so one character stays one character and offsets line
/// up with the text.
fn fold(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// For each character of `text`, whether a word starts there: the first
/// character, a letter or digit after anything else, or an upper-case
/// letter after a lower-case one ("SyncAll").
fn word_starts(text: &str) -> Vec<bool> {
    let mut previous: Option<char> = None;
    text.chars()
        .map(|ch| {
            let start = match previous {
                None => true,
                Some(previous) => {
                    (ch.is_alphanumeric() && !previous.is_alphanumeric())
                        || (ch.is_uppercase() && previous.is_lowercase())
                }
            };
            previous = Some(ch);
            start
        })
        .collect()
}

/// The best-scoring way to match `needle` as a subsequence of `haystack`,
/// with the matched character indices, or `None` when it does not match.
fn best_placement(
    needle: &[char],
    haystack: &[(usize, char)],
    starts: &[bool],
) -> Option<(i32, Vec<usize>)> {
    const NONE: i32 = i32::MIN / 2;
    let (rows, columns) = (needle.len(), haystack.len());
    if rows == 0 {
        return Some((0, Vec::new()));
    }
    if rows > columns {
        return None;
    }
    let bonus = |column: usize| {
        let mut bonus = MATCH;
        if starts[column] {
            bonus += WORD_START;
        }
        bonus
    };

    // score[row][column]: the best score with needle[row] matched at
    // haystack[column]; from[row][column]: where needle[row - 1] matched.
    let mut score = vec![vec![NONE; columns]; rows];
    let mut from = vec![vec![usize::MAX; columns]; rows];
    for (column, &(_, ch)) in haystack.iter().enumerate() {
        if ch == needle[0] {
            let leading = (column as i32 * LEADING).min(MAX_LEADING_PENALTY);
            let text_start = if column == 0 { TEXT_START } else { 0 };
            score[0][column] = bonus(column) + text_start - leading;
        }
    }
    for row in 1..rows {
        // The best of score[row - 1][k] - GAP * (column - 1 - k) over
        // k <= column - 2, and the k it came from.
        let mut running = (NONE, usize::MAX);
        for column in 0..columns {
            let adjacent = if column > 0 {
                score[row - 1][column - 1]
            } else {
                NONE
            };
            if haystack[column].1 == needle[row] {
                let mut best = running;
                if adjacent > NONE && adjacent + CONSECUTIVE > best.0 {
                    best = (adjacent + CONSECUTIVE, column - 1);
                }
                if best.0 > NONE {
                    score[row][column] = best.0 + bonus(column);
                    from[row][column] = best.1;
                }
            }
            // Slide the window on: k = column - 1 becomes eligible for the
            // next column, and every candidate is one letter further away.
            let carried = if running.0 > NONE {
                (running.0 - GAP, running.1)
            } else {
                running
            };
            running = if column > 0 && adjacent > NONE && adjacent - GAP > carried.0 {
                (adjacent - GAP, column - 1)
            } else {
                carried
            };
        }
    }

    let (end, &best) = score[rows - 1]
        .iter()
        .enumerate()
        .max_by_key(|&(column, &value)| (value, std::cmp::Reverse(column)))?;
    if best <= NONE {
        return None;
    }
    let mut indices = vec![end; rows];
    for row in (1..rows).rev() {
        indices[row - 1] = from[row][indices[row]];
    }
    Some((best, indices))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, text: &str) -> Option<i32> {
        fuzzy_match(query, text).map(|found| found.score)
    }

    fn highlighted(query: &str, text: &str) -> String {
        let found = fuzzy_match(query, text).expect("it matches");
        text.char_indices()
            .filter(|(offset, _)| found.positions.contains(offset))
            .map(|(_, ch)| ch)
            .collect()
    }

    #[test]
    fn letters_must_appear_in_order() {
        assert!(score("arch", "Archive").is_some());
        assert!(score("ARCH", "archive").is_some(), "case is ignored");
        assert!(score("hcra", "Archive").is_none());
        assert!(score("archives", "Archive").is_none());
        assert!(score("x", "").is_none());
    }

    #[test]
    fn a_blank_query_matches_everything() {
        assert_eq!(score("", "Archive"), Some(0));
        assert_eq!(score("   ", "Archive"), Some(0));
        assert_eq!(
            fuzzy_match("", "").map(|found| found.positions),
            Some(vec![])
        );
    }

    #[test]
    fn every_word_must_match_in_any_order() {
        assert!(score("arc gt", "Go to Archive").is_some());
        assert!(score("arc work", "Go to Archive Work").is_some());
        assert!(score("arc home", "Go to Archive Work").is_none());
    }

    #[test]
    fn word_starts_and_runs_beat_scattered_letters() {
        // "Mark Read / Unread" starts both words with the letters typed.
        assert!(score("mr", "Mark Read / Unread") > score("mr", "Summary"));
        // A prefix beats the same letters inside a word.
        assert!(score("arch", "Archive") > score("arch", "Search"));
        // A run beats the same letters spread out.
        assert!(score("sync", "Sync Account") > score("sync", "Save Your Notes Carefully"));
        // An exact command name beats a longer one containing it.
        assert!(score("archive", "Archive") > score("archive", "Go to Archive"));
    }

    #[test]
    fn the_best_placement_is_highlighted() {
        assert_eq!(highlighted("inbox", "Go to Unified Inbox"), "Inbox");
        assert_eq!(highlighted("ui", "Go to Unified Inbox"), "UI");
        assert_eq!(highlighted("sa", "Sync All Accounts"), "SA");
        assert_eq!(highlighted("rep all", "Reply All"), "RepAll");
    }

    #[test]
    fn camel_case_counts_as_word_starts() {
        assert_eq!(highlighted("sa", "SyncAll"), "SA");
    }

    #[test]
    fn non_ascii_text_keeps_byte_offsets() {
        let found = fuzzy_match("ges", "Gesendet Objekte").expect("it matches");
        assert_eq!(found.positions, vec![0, 1, 2]);
        let found = fuzzy_match("ö", "Gelöscht").expect("it matches");
        assert_eq!(found.positions, vec!["Gel".len()]);
        let found = fuzzy_match("Ö", "Gelöscht").expect("case folds");
        assert_eq!(found.positions, vec!["Gel".len()]);
    }
}
