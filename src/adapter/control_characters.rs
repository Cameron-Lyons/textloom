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
    if text.chars().all(allowed) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(
            text.chars()
                .filter(|character| allowed(*character))
                .collect(),
        )
    }
}
