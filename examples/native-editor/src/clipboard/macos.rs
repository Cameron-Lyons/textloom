use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use super::{MAX_NATIVE_BYTES, NativeItem};

const PLAIN_TYPE: &str = "public.utf8-plain-text";
const HTML_TYPE: &str = "public.html";
const RICH_TYPE: &str = "org.textloom.fragment";

struct Types {
    plain: Retained<NSString>,
    html: Retained<NSString>,
    rich: Retained<NSString>,
}

impl Types {
    fn new() -> Self {
        Self {
            plain: NSString::from_str(PLAIN_TYPE),
            html: NSString::from_str(HTML_TYPE),
            rich: NSString::from_str(RICH_TYPE),
        }
    }
}

struct ItemData {
    plain: Retained<NSData>,
    html: Retained<NSData>,
    rich: Retained<NSData>,
}

fn bounded_item(board: &NSPasteboard, types: &Types) -> Result<Option<ItemData>, String> {
    let Some(items) = board.pasteboardItems() else {
        return Ok(None);
    };
    if items.count() != 1 {
        return Ok(None);
    }
    let Some(item) = items.firstObject() else {
        return Ok(None);
    };
    let Some(rich) = item.dataForType(&types.rich) else {
        return Ok(None);
    };
    let plain = item
        .dataForType(&types.plain)
        .ok_or_else(|| "Native clipboard item is missing plain text".to_owned())?;
    let html = item
        .dataForType(&types.html)
        .ok_or_else(|| "Native clipboard item is missing HTML".to_owned())?;

    if [plain.length(), html.length(), rich.length()]
        .into_iter()
        .any(|length| length > MAX_NATIVE_BYTES)
    {
        return Err("Native clipboard item exceeds the size limit".to_owned());
    }
    Ok(Some(ItemData { plain, html, rich }))
}

pub(super) fn publish(item: &NativeItem, _owner: usize) -> Result<(), String> {
    autoreleasepool(|_| publish_to(&NSPasteboard::generalPasteboard(), item))
}

fn publish_to(board: &NSPasteboard, item: &NativeItem) -> Result<(), String> {
    if [item.plain.len(), item.html.len(), item.rich.len()]
        .into_iter()
        .any(|length| length > MAX_NATIVE_BYTES)
    {
        return Err("Native clipboard item exceeds the size limit".to_owned());
    }
    autoreleasepool(|_| {
        let types = Types::new();
        let data = ItemData {
            plain: NSData::with_bytes(item.plain.as_bytes()),
            html: NSData::with_bytes(item.html.as_bytes()),
            rich: NSData::with_bytes(&item.rich),
        };
        let native_item = NSPasteboardItem::new();
        if !native_item.setData_forType(&data.plain, &types.plain)
            || !native_item.setData_forType(&data.html, &types.html)
            || !native_item.setData_forType(&data.rich, &types.rich)
        {
            return Err("Could not prepare native clipboard item".to_owned());
        }
        let writer: &ProtocolObject<dyn NSPasteboardWriting> =
            ProtocolObject::from_ref(&*native_item);
        let objects = NSArray::from_slice(&[writer]);
        let cleared_generation = board.clearContents();
        if board.changeCount() != cleared_generation {
            return Err("Native clipboard changed during publication".to_owned());
        }
        if !board.writeObjects(&objects) {
            return Err("Could not publish native clipboard item".to_owned());
        }

        let generation = board.changeCount();
        let result = bounded_item(board, &types).and_then(|published| {
            let Some(published) = published else {
                return Err("Could not verify native clipboard publication".to_owned());
            };
            if !published.plain.isEqualToData(&data.plain)
                || !published.html.isEqualToData(&data.html)
                || !published.rich.isEqualToData(&data.rich)
            {
                return Err("Could not verify native clipboard publication".to_owned());
            }
            Ok(())
        });
        if board.changeCount() != generation {
            return Err("Native clipboard changed during publication".to_owned());
        }
        result
    })
}

pub(super) fn read() -> Result<Option<NativeItem>, String> {
    autoreleasepool(|_| read_from(&NSPasteboard::generalPasteboard()))
}

fn read_from(board: &NSPasteboard) -> Result<Option<NativeItem>, String> {
    autoreleasepool(|_| {
        let generation = board.changeCount();
        let result = bounded_item(board, &Types::new()).and_then(|captured| {
            let Some(captured) = captured else {
                return Ok(None);
            };
            let plain = String::from_utf8(captured.plain.to_vec())
                .map_err(|_| "Native clipboard plain text is not UTF-8".to_owned())?;
            let html = String::from_utf8(captured.html.to_vec())
                .map_err(|_| "Native clipboard HTML is not UTF-8".to_owned())?;
            Ok(Some(NativeItem {
                plain,
                html,
                rich: captured.rich.to_vec(),
            }))
        });
        if board.changeCount() != generation {
            return Err("Native clipboard changed during capture".to_owned());
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use textloom::{Color, Editor, Fragment, StylePatch};

    use super::*;

    // Use a unique named pasteboard: these tests must never read or replace the
    // user's general clipboard, including when running concurrently in CI.
    struct TestPasteboard(Retained<NSPasteboard>);

    impl TestPasteboard {
        fn new() -> Self {
            Self(NSPasteboard::pasteboardWithUniqueName())
        }
    }

    impl Drop for TestPasteboard {
        fn drop(&mut self) {
            self.0.clearContents();
        }
    }

    #[test]
    fn native_formats_preserve_unicode_styles_and_replaced_publications() {
        autoreleasepool(|_| {
            let board = TestPasteboard::new();
            let mut editor = Editor::from_text("café 日本語 e\u{301} 👩🏽‍💻\nsecond");
            editor.select_all();
            editor
                .apply_style(StylePatch {
                    bold: Some(true),
                    background: Some(Some(Color::YELLOW)),
                    ..Default::default()
                })
                .unwrap();
            let fragment = Fragment::from_document(editor.document());
            for _ in 0..2 {
                // Identical text still represents a fresh publication.
                let item = NativeItem::from_fragment(&fragment).unwrap();
                let generation = board.0.changeCount();
                publish_to(&board.0, &item).unwrap();
                assert_ne!(board.0.changeCount(), generation);
                let captured = read_from(&board.0).unwrap().unwrap();
                assert_eq!(captured, item);
                assert_eq!(captured.fragment().unwrap(), fragment);

                // Inspect each native representation independently of read_from.
                let items = board.0.pasteboardItems().unwrap();
                assert_eq!(items.count(), 1);
                let native_item = items.firstObject().unwrap();
                for (data_type, expected) in [
                    (PLAIN_TYPE, item.plain.as_bytes()),
                    (HTML_TYPE, item.html.as_bytes()),
                    (RICH_TYPE, item.rich.as_slice()),
                ] {
                    let data = native_item
                        .dataForType(&NSString::from_str(data_type))
                        .unwrap();
                    assert_eq!(data.to_vec(), expected, "{data_type}");
                }
                assert!(item.html.contains("<meta charset=\"utf-8\">"));
            }
        });
    }

    #[test]
    fn empty_external_and_multiple_items_use_plain_fallback() {
        autoreleasepool(|_| {
            let board = TestPasteboard::new();
            board.0.clearContents();
            assert!(read_from(&board.0).unwrap().is_none());

            let external = NSPasteboardItem::new();
            assert!(external.setData_forType(
                &NSData::with_bytes("external café 日本語".as_bytes()),
                &NSString::from_str(PLAIN_TYPE),
            ));
            let writer: &ProtocolObject<dyn NSPasteboardWriting> =
                ProtocolObject::from_ref(&*external);
            assert!(board.0.writeObjects(&NSArray::from_slice(&[writer])));
            assert!(read_from(&board.0).unwrap().is_none());

            board.0.clearContents();
            // AppKit binds an item to its first publication; never publish the
            // same NSPasteboardItem again, even after clearing that pasteboard.
            let first = NSPasteboardItem::new();
            let rich = NativeItem::from_fragment(&Fragment::from_text("first")).unwrap();
            for (data_type, bytes) in [
                (PLAIN_TYPE, rich.plain.as_bytes()),
                (HTML_TYPE, rich.html.as_bytes()),
                (RICH_TYPE, rich.rich.as_slice()),
            ] {
                assert!(
                    first.setData_forType(
                        &NSData::with_bytes(bytes),
                        &NSString::from_str(data_type),
                    )
                );
            }
            let first_writer: &ProtocolObject<dyn NSPasteboardWriting> =
                ProtocolObject::from_ref(&*first);
            let second = NSPasteboardItem::new();
            assert!(second.setData_forType(
                &NSData::with_bytes(b"second"),
                &NSString::from_str(PLAIN_TYPE),
            ));
            let second_writer: &ProtocolObject<dyn NSPasteboardWriting> =
                ProtocolObject::from_ref(&*second);
            assert!(
                board
                    .0
                    .writeObjects(&NSArray::from_slice(&[first_writer, second_writer]))
            );
            assert!(read_from(&board.0).unwrap().is_none());
        });
    }

    #[test]
    fn incomplete_or_invalid_native_items_are_rejected() {
        autoreleasepool(|_| {
            let board = TestPasteboard::new();
            for plain in [None, Some(b"\xff".as_slice())] {
                let item = NSPasteboardItem::new();
                assert!(
                    item.setData_forType(
                        &NSData::with_bytes(b"rich"),
                        &NSString::from_str(RICH_TYPE),
                    )
                );
                assert!(item.setData_forType(
                    &NSData::with_bytes(b"<p>html</p>"),
                    &NSString::from_str(HTML_TYPE),
                ));
                if let Some(plain) = plain {
                    assert!(item.setData_forType(
                        &NSData::with_bytes(plain),
                        &NSString::from_str(PLAIN_TYPE),
                    ));
                }
                let writer: &ProtocolObject<dyn NSPasteboardWriting> =
                    ProtocolObject::from_ref(&*item);
                board.0.clearContents();
                assert!(board.0.writeObjects(&NSArray::from_slice(&[writer])));
                assert!(read_from(&board.0).is_err());
            }
        });
    }
}
