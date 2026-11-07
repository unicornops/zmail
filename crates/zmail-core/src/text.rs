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

/// Formats a count with thousands separators, for example `85,000`.
///
/// English grouping only, like [`count_noun`].
pub fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::{count_noun, thousands};

    #[test]
    fn count_noun_picks_the_singular_only_for_one() {
        assert_eq!(count_noun(0, "message", "messages"), "0 messages");
        assert_eq!(count_noun(1, "message", "messages"), "1 message");
        assert_eq!(count_noun(2, "message", "messages"), "2 messages");
        assert_eq!(count_noun(1, "identity", "identities"), "1 identity");
        assert_eq!(count_noun(5, "identity", "identities"), "5 identities");
    }

    #[test]
    fn thousands_groups_digits_in_threes() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(85_000), "85,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
