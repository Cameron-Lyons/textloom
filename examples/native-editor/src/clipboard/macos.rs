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
        let board = NSPasteboard::generalPasteboard();
        let cleared_generation = board.clearContents();
        if board.changeCount() != cleared_generation {
            return Err("Native clipboard changed during publication".to_owned());
        }
        if !board.writeObjects(&objects) {
            return Err("Could not publish native clipboard item".to_owned());
        }

        let generation = board.changeCount();
        let result = bounded_item(&board, &types).and_then(|published| {
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
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let generation = board.changeCount();
        let result = bounded_item(&board, &Types::new()).and_then(|captured| {
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
