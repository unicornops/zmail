//! Small text helpers shared by the shell and the CLI.

/// Formats a count with the matching singular or plural noun, for example
/// `1 message` or `3 messages`.
///
/// English-only for now; the plural form is passed in rather than derived
/// so irregular nouns ("1 identity", "2 identities") read correctly.
pub fn count_noun(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::count_noun;

    #[test]
    fn count_noun_picks_the_singular_only_for_one() {
        assert_eq!(count_noun(0, "message", "messages"), "0 messages");
        assert_eq!(count_noun(1, "message", "messages"), "1 message");
        assert_eq!(count_noun(2, "message", "messages"), "2 messages");
        assert_eq!(count_noun(1, "identity", "identities"), "1 identity");
        assert_eq!(count_noun(5, "identity", "identities"), "5 identities");
    }
}
