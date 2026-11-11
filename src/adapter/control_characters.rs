//! Filtering of control characters out of text that adapters insert.

use std::borrow::Cow;

/// Which control characters [`clean_text`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ControlCharacters {
    /// Keeps line feeds, carriage returns, and tabs, which pasted and
    /// IME-committed text may contain.
    KeepWhitespace,
    /// Removes every control character, as typed text should have none.
    RemoveAll,
}

/// Removes control characters from text that an adapter is about to insert,
/// except the whitespace that `keep` allows.
///
/// Returns `text` unchanged, without allocating, when nothing is removed.
pub(super) fn clean_text(text: &str, keep: ControlCharacters) -> Cow<'_, str> {
    let allowed = |character: char| {
        !character.is_control()
            || keep == ControlCharacters::KeepWhitespace && matches!(character, '\n' | '\r' | '\t')
    };
    let Some((start, _)) = text
        .char_indices()
        .find(|(_, character)| !allowed(*character))
    else {
        return Cow::Borrowed(text);
    };
    // The prefix is already valid; copy it without decoding every character again.
    let mut clean = String::from(&text[..start]);
    clean.extend(
        text[start..]
            .chars()
            .filter(|character| allowed(*character)),
    );
    Cow::Owned(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_filtering_preserves_unicode_and_allowed_whitespace() {
        let text = "\0café\u{85} 👩🏽‍💻\u{7f}\r\n\t日本\u{9f}\u{2028}end\u{1b}";
        assert_eq!(
            clean_text(text, ControlCharacters::KeepWhitespace),
            "café 👩🏽‍💻\r\n\t日本\u{2028}end"
        );
        assert_eq!(
            clean_text(text, ControlCharacters::RemoveAll),
            "café 👩🏽‍💻日本\u{2028}end"
        );
        // Every control can occur after a multibyte prefix or before the first character.
        for control in ('\0'..='\u{1f}').chain('\u{7f}'..='\u{9f}') {
            for prefix in ["", "日本👩🏽‍💻"] {
                let text = format!("{prefix}{control}café");
                assert_eq!(
                    clean_text(&text, ControlCharacters::RemoveAll),
                    format!("{prefix}café")
                );
            }
        }
    }

    #[test]
    fn clean_text_stays_borrowed_without_removing_unicode_separators() {
        for text in ["", "typed text", "café 日本 👩🏽‍💻", "\u{a0}\u{2028}\u{2029}"] {
            for mode in [
                ControlCharacters::RemoveAll,
                ControlCharacters::KeepWhitespace,
            ] {
                assert!(matches!(clean_text(text, mode), Cow::Borrowed(value) if value == text));
            }
        }
        let whitespace = "\tfirst\r\nsecond\n";
        assert!(matches!(
            clean_text(whitespace, ControlCharacters::KeepWhitespace),
            Cow::Borrowed(value) if value == whitespace
        ));
    }
}
