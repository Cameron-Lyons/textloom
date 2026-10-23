//! Helpers for showing parts of long tool output.

/// The last `count` characters of `text`.
#[must_use]
pub fn tail(text: &str, count: usize) -> &str {
    let skip = text.chars().count().saturating_sub(count);
    text.char_indices()
        .nth(skip)
        .map_or("", |(index, _)| &text[index..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_counts_characters() {
        assert_eq!(tail("café", 2), "fé");
        assert_eq!(tail("café", 10), "café");
        assert_eq!(tail("café", 0), "");
    }
}
