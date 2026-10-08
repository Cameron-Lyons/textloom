//! Literal and Unicode lowercase search coordinates and boundary contracts.

use textloom::{Document, Position, SearchOptions};

fn default_options() -> SearchOptions {
    SearchOptions::default()
}

#[test]
fn literal_matches_are_ordered_nonoverlapping_and_empty_queries_match_nothing() {
    let document = Document::from_text("banana ana\nana");
    assert_eq!(
        document.find("ana", default_options()),
        vec![
            Position::new(0, 1)..Position::new(0, 4),
            Position::new(0, 7)..Position::new(0, 10),
            Position::new(1, 0)..Position::new(1, 3),
        ]
    );
    assert!(document.find("", default_options()).is_empty());
    assert!(Document::new().find("a", default_options()).is_empty());
    assert!(document.find("BANANA", default_options()).is_empty());
}

#[test]
fn paragraph_local_literals_preserve_empty_paragraphs_and_unicode_boundaries() {
    let document = Document::from_text("\na\nba aa\n\ne\u{301} e\n🇨🇺🇺🇺\na\n");
    assert_eq!(
        document.find("a", default_options()),
        vec![
            Position::new(1, 0)..Position::new(1, 1),
            Position::new(2, 1)..Position::new(2, 2),
            Position::new(2, 3)..Position::new(2, 4),
            Position::new(2, 4)..Position::new(2, 5),
            Position::new(6, 0)..Position::new(6, 1),
        ]
    );
    assert_eq!(
        document.find("aa", default_options()),
        vec![Position::new(2, 3)..Position::new(2, 5)]
    );
    assert_eq!(
        document.find("e", default_options()),
        vec![Position::new(4, 4)..Position::new(4, 5)]
    );
    assert_eq!(
        document.find("🇺🇺", default_options()),
        vec![Position::new(5, 8)..Position::new(5, 16)]
    );
    assert!(document.find("aa aa aa", default_options()).is_empty());
}

#[test]
fn queries_normalize_newlines_and_preserve_empty_paragraph_coordinates() {
    let document = Document::from_text("first\n\nlast\n");
    assert_eq!(
        document.find("st\r\n\rla", default_options()),
        vec![Position::new(0, 3)..Position::new(2, 2)]
    );
    assert_eq!(
        document.find("\r\n", default_options()),
        vec![
            Position::new(0, 5)..Position::new(1, 0),
            Position::new(1, 0)..Position::new(2, 0),
            Position::new(2, 4)..Position::new(3, 0),
        ]
    );
}

#[test]
fn only_complete_graphemes_match() {
    let document = Document::from_text("e\u{301} e 👩‍💻 👩 🇺🇸 🇺");
    assert_eq!(
        document.find("e", default_options()),
        vec![Position::new(0, 4)..Position::new(0, 5)]
    );
    assert!(document.find("\u{301}", default_options()).is_empty());
    assert_eq!(document.find("e\u{301}", default_options()).len(), 1);
    let women = document.find("👩", default_options());
    assert_eq!(women.len(), 1);
    assert_eq!(document.text(women[0].clone()).unwrap(), "👩");
    let flags = document.find("🇺", default_options());
    assert_eq!(flags.len(), 1);
    assert_eq!(document.text(flags[0].clone()).unwrap(), "🇺");
    assert_eq!(document.find("👩‍💻", default_options()).len(), 1);
    // The first literal candidate overlaps two flags. Rejecting it must
    // still allow the later candidate that covers one complete flag.
    assert_eq!(
        Document::from_text("🇨🇺🇺🇺").find("🇺🇺", default_options()),
        vec![Position::new(0, 8)..Position::new(0, 16)]
    );
}

#[test]
fn insensitive_search_maps_original_bytes_and_rejects_partial_expansions() {
    let document = Document::from_text("İ i İ\nCAFÉ café ΟΣ");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    assert_eq!(
        document.find("i", options),
        vec![Position::new(0, 3)..Position::new(0, 4)]
    );
    assert!(document.find("\u{307}", options).is_empty());
    assert_eq!(
        document.find("i\u{307}", options),
        vec![
            Position::new(0, 0)..Position::new(0, 2),
            Position::new(0, 5)..Position::new(0, 7),
        ]
    );
    let cafes = document.find("café", options);
    assert_eq!(cafes.len(), 2);
    assert_eq!(document.text(cafes[0].clone()).unwrap(), "CAFÉ");
    assert_eq!(document.text(cafes[1].clone()).unwrap(), "café");
    let greek = document.find("ος", options);
    assert_eq!(greek.len(), 1);
    assert_eq!(document.text(greek[0].clone()).unwrap(), "ΟΣ");
    assert!(
        Document::from_text("Straße")
            .find("STRASSE", options)
            .is_empty()
    );
    assert!(
        Document::from_text("é")
            .find("e\u{301}", options)
            .is_empty()
    );
}

#[test]
fn whole_words_use_unicode_word_boundaries() {
    let document = Document::from_text("cat cats bobcat cat's cat.\nCAT café cafés 你好世界");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: true,
    };
    let matches = document.find("cat", options);
    assert_eq!(matches.len(), 3);
    assert_eq!(document.text(matches[0].clone()).unwrap(), "cat");
    assert_eq!(document.text(matches[1].clone()).unwrap(), "cat");
    assert_eq!(document.text(matches[2].clone()).unwrap(), "CAT");
    assert_eq!(document.find("café", options).len(), 1);
    assert_eq!(document.find("你", options).len(), 1);
    assert!(document.find("af", options).is_empty());
}

#[test]
fn case_insensitive_cross_paragraph_queries_map_original_coordinates() {
    let document = Document::from_text("İ\nCAFÉ");
    let matches = document.find(
        "i\u{307}\rcafé",
        SearchOptions {
            case_sensitive: false,
            whole_word: true,
        },
    );
    assert_eq!(matches, vec![Position::new(0, 0)..Position::new(1, 5)]);
    assert_eq!(document.text(matches[0].clone()).unwrap(), "İ\nCAFÉ");
}

#[test]
fn lowercase_byte_width_changes_map_before_between_and_after_matches() {
    let document = Document::from_text("KȺİ Kⱥi\u{307}");
    let options = SearchOptions {
        case_sensitive: false,
        whole_word: false,
    };
    assert_eq!(
        document.find("k", options),
        vec![
            Position::new(0, 0)..Position::new(0, 3),
            Position::new(0, 8)..Position::new(0, 9)
        ]
    );
    assert_eq!(
        document.find("ⱥ", options),
        vec![
            Position::new(0, 3)..Position::new(0, 5),
            Position::new(0, 9)..Position::new(0, 12)
        ]
    );
    assert_eq!(
        document.find("i\u{307}", options),
        vec![
            Position::new(0, 5)..Position::new(0, 7),
            Position::new(0, 12)..Position::new(0, 15)
        ]
    );
    assert!(document.find("i", options).is_empty());
}

#[test]
fn overlapping_retries_preserve_earlier_and_later_matches() {
    for source in ["🇺🇺 🇨🇺🇺🇺 🇺🇺", "🇺🇺\n🇨🇺🇺🇺\n🇺🇺"] {
        let document = Document::from_text(source);
        let matches = document.find("🇺🇺", SearchOptions::default());
        assert_eq!(matches.len(), 3);
        for range in &matches {
            assert_eq!(document.text(range.clone()).unwrap(), "🇺🇺");
        }
        assert!(matches.windows(2).all(|pair| pair[0].end <= pair[1].start));
    }

    let document = Document::from_text("i İ i\ni İ i");
    let matches = document.find(
        "i",
        SearchOptions {
            case_sensitive: false,
            whole_word: false,
        },
    );
    assert_eq!(matches.len(), 4);
    for range in matches {
        assert_eq!(document.text(range).unwrap(), "i");
    }

    assert_eq!(
        Document::from_text("a-a aa-a-a a-a").find(
            "a-a",
            SearchOptions {
                whole_word: true,
                ..Default::default()
            }
        ),
        vec![
            Position::new(0, 0)..Position::new(0, 3),
            Position::new(0, 7)..Position::new(0, 10),
            Position::new(0, 11)..Position::new(0, 14),
        ]
    );
}
