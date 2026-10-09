//! Windows rich clipboard operations. The parent runs these in a bounded helper process.

use std::thread;
use std::time::Duration;

use clipboard_win::{Clipboard, formats, raw};

use super::{MAX_NATIVE_BYTES, MIME, NativeItem};

const OPEN_ATTEMPTS: usize = 10;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);
const HTML_FORMAT: &str = "HTML Format";
const HTML_HEADER: &str = "Version:1.0\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
const HTML_PREFIX: &str = "<html><body><!--StartFragment-->";
const HTML_SUFFIX: &str = "<!--EndFragment--></body></html>";
const MAX_HTML_HEADER_BYTES: usize = 8192;

pub fn publish(item: &NativeItem, owner: usize) -> Result<(), String> {
    if owner == 0 {
        return Err("Windows rich clipboard copy requires a native window owner".into());
    }
    checked_size(item.rich.len(), "rich clipboard")?;
    if item.rich.is_empty() {
        return Err("Windows rich clipboard copy requires a framed rich payload".into());
    }
    // Prepare and validate all buffers before emptying the existing clipboard.
    let plain = encode_unicode(&item.plain)?;
    let html = encode_html(&item.html)?;
    let rich_format = registered_format(MIME, "rich")?;
    let html_format = registered_format(HTML_FORMAT, "HTML")?;
    let native_formats = [
        (rich_format, item.rich.as_slice(), "rich"),
        (html_format, html.as_slice(), "HTML"),
        (formats::CF_UNICODETEXT, plain.as_slice(), "Unicode text"),
    ];

    with_clipboard(owner, || {
        raw::empty().map_err(|error| format!("Windows clipboard clear failed: {error}"))?;
        if raw::get_owner().map(|window| window.as_ptr() as usize) != Some(owner) {
            return Err("Windows clipboard owner verification failed".into());
        }
        // Register the most descriptive representations first for format enumeration.
        for (format, bytes, label) in native_formats {
            raw::set_without_clear(format, bytes).map_err(|error| {
                format!("Windows clipboard {label} publication failed: {error}")
            })?;
        }
        for (format, expected, label) in native_formats {
            let actual = read_bytes(format, label)?;
            // GlobalAlloc may round up the allocation; clipboard-win uses zeroed memory.
            if !actual.starts_with(expected)
                || actual[expected.len()..].iter().any(|byte| *byte != 0)
            {
                return Err(format!("Windows clipboard {label} verification failed"));
            }
        }
        Ok(())
    })
}

pub fn read() -> Result<Option<NativeItem>, String> {
    let rich_format = registered_format(MIME, "rich")?;
    let html_format = registered_format(HTML_FORMAT, "HTML")?;
    with_clipboard(0, || {
        if !raw::is_format_avail(rich_format) {
            return Ok(None);
        }
        let rich = read_bytes(rich_format, "rich")?;
        let plain = if raw::is_format_avail(formats::CF_UNICODETEXT) {
            decode_unicode(&read_bytes(formats::CF_UNICODETEXT, "Unicode text")?)?
        } else {
            String::new()
        };
        let html = if raw::is_format_avail(html_format) {
            decode_html(&read_bytes(html_format, "HTML")?)?
        } else {
            String::new()
        };
        Ok(Some(NativeItem { plain, html, rich }))
    })
}

fn registered_format(name: &str, label: &str) -> Result<u32, String> {
    raw::register_format(name)
        .map(std::num::NonZeroU32::get)
        .ok_or_else(|| format!("Windows clipboard {label} format registration failed"))
}

fn with_clipboard<T>(
    owner: usize,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let guard = open_clipboard(owner)?;
    let result = operation();
    // Clipboard's Drop ignores CloseClipboard errors. Close explicitly so success means
    // the three verified representations were also made available to other processes.
    let close = raw::close().map_err(|error| format!("Windows clipboard close failed: {error}"));
    if close.is_ok() {
        std::mem::forget(guard);
    }
    // On a close error the guard gets one final cleanup attempt as it leaves scope.
    match (result, close) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(close_error)) => Err(format!("{error}; {close_error}")),
    }
}

fn open_clipboard(owner: usize) -> Result<Clipboard, String> {
    for attempt in 0..OPEN_ATTEMPTS {
        match Clipboard::new_for(owner as clipboard_win::types::HWND) {
            Ok(guard) => return Ok(guard),
            Err(error) if attempt + 1 == OPEN_ATTEMPTS => {
                return Err(format!(
                    "Windows clipboard open failed after finite retries: {error}"
                ));
            }
            Err(_) => thread::sleep(OPEN_RETRY_DELAY),
        }
    }
    Err("Windows clipboard open attempts exhausted".into())
}

fn checked_size(size: usize, label: &str) -> Result<usize, String> {
    if size > MAX_NATIVE_BYTES {
        Err(format!(
            "Windows {label} exceeds the native clipboard byte limit"
        ))
    } else {
        Ok(size)
    }
}

fn byte_buffer(size: usize, label: &str) -> Result<Vec<u8>, String> {
    checked_size(size, label)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| format!("Windows {label} allocation failed"))?;
    Ok(bytes)
}

fn read_bytes(format: u32, label: &str) -> Result<Vec<u8>, String> {
    // These safe APIs inspect native GlobalSize before any buffer allocation, then copy
    // into a caller-sized slice. get_vec/get_string/get_html would allocate unchecked.
    let size = raw::size(format)
        .ok_or_else(|| format!("Windows clipboard {label} native byte size is unavailable"))?
        .get();
    let mut bytes = byte_buffer(size, label)?;
    bytes.resize(size, 0);
    let copied = raw::get(format, &mut bytes)
        .map_err(|error| format!("Windows clipboard {label} read failed: {error}"))?;
    if copied != size || raw::size(format).map(std::num::NonZeroUsize::get) != Some(size) {
        return Err(format!(
            "Windows clipboard {label} changed during the bounded read"
        ));
    }
    Ok(bytes)
}

fn encode_unicode(text: &str) -> Result<Vec<u8>, String> {
    if text.contains('\0') {
        return Err("Windows Unicode clipboard text contains a NUL character".into());
    }
    let mut units = 1_usize; // Terminator.
    let mut previous = None;
    for unit in text.encode_utf16() {
        units = units
            .checked_add(1 + usize::from(unit == 10 && previous != Some(13)))
            .ok_or("Windows Unicode clipboard byte size overflow")?;
        previous = Some(unit);
    }
    let size = units
        .checked_mul(2)
        .ok_or("Windows Unicode clipboard byte size overflow")?;
    let mut bytes = byte_buffer(size, "Unicode text")?;
    previous = None;
    for unit in text.encode_utf16() {
        if unit == 10 && previous != Some(13) {
            bytes.extend_from_slice(&13_u16.to_le_bytes());
        }
        bytes.extend_from_slice(&unit.to_le_bytes());
        previous = Some(unit);
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    Ok(bytes)
}

fn decode_unicode(bytes: &[u8]) -> Result<String, String> {
    let (units, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("Windows Unicode clipboard data has an odd native byte size".into());
    }
    let end = units
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or("Windows Unicode clipboard data has no NUL terminator")?;
    let units = units[..end].iter().map(|unit| u16::from_le_bytes(*unit));
    let size = char::decode_utf16(units.clone()).try_fold(0_usize, |size, character| {
        let character =
            character.map_err(|_| "Windows Unicode clipboard data is invalid UTF-16")?;
        size.checked_add(character.len_utf8())
            .ok_or("Windows Unicode clipboard UTF-8 byte size overflow")
    })?;
    checked_size(size, "decoded Unicode text")?;
    let mut text = String::new();
    text.try_reserve_exact(size)
        .map_err(|_| "Windows Unicode clipboard text allocation failed")?;
    for character in char::decode_utf16(units) {
        let character =
            character.map_err(|_| "Windows Unicode clipboard data is invalid UTF-16")?;
        if character == '\n' && text.ends_with('\r') {
            text.pop();
        }
        text.push(character);
    }
    Ok(text)
}

fn encode_html(fragment: &str) -> Result<Vec<u8>, String> {
    if fragment.contains('\0') {
        return Err("Windows HTML clipboard fragment contains a NUL character".into());
    }
    let start_html = HTML_HEADER.len();
    let start_fragment = start_html + HTML_PREFIX.len();
    let end_fragment = start_fragment
        .checked_add(fragment.len())
        .ok_or("Windows HTML clipboard byte size overflow")?;
    let end_html = end_fragment
        .checked_add(HTML_SUFFIX.len())
        .ok_or("Windows HTML clipboard byte size overflow")?;
    let size = end_html
        .checked_add(1)
        .ok_or("Windows HTML clipboard byte size overflow")?;
    let mut bytes = byte_buffer(size, "HTML")?;
    // CF_HTML offsets count UTF-8 bytes from the beginning, excluding the terminator.
    let header = format!(
        "Version:1.0\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    );
    if header.len() != HTML_HEADER.len() {
        return Err("Windows HTML clipboard offset width overflow".into());
    }
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(HTML_PREFIX.as_bytes());
    bytes.extend_from_slice(fragment.as_bytes());
    bytes.extend_from_slice(HTML_SUFFIX.as_bytes());
    bytes.push(0);
    Ok(bytes)
}

fn decode_html(bytes: &[u8]) -> Result<String, String> {
    let mut offsets = [None; 4];
    let mut version = false;
    let mut header_end = 0;
    let header = &bytes[..bytes.len().min(MAX_HTML_HEADER_BYTES)];
    for line in header.split_inclusive(|byte| *byte == b'\r' || *byte == b'\n') {
        header_end += line.len();
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            break;
        };
        let key = &line[..separator];
        let value = &line[separator + 1..];
        if key == b"Version" {
            if version || !matches!(value, b"0.9" | b"1.0") {
                return Err("Windows HTML clipboard header has an invalid version".into());
            }
            version = true;
            continue;
        }
        let index = match key {
            b"StartHTML" => 0,
            b"EndHTML" => 1,
            b"StartFragment" => 2,
            b"EndFragment" => 3,
            _ => continue,
        };
        if offsets[index].is_some() {
            return Err("Windows HTML clipboard header has duplicate offsets".into());
        }
        offsets[index] = Some(
            std::str::from_utf8(value.trim_ascii())
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or("Windows HTML clipboard header has an invalid byte offset")?,
        );
        if offsets.iter().all(Option::is_some) {
            break;
        }
    }
    let [
        Some(start_html),
        Some(end_html),
        Some(start_fragment),
        Some(end_fragment),
    ] = offsets
    else {
        return Err("Windows HTML clipboard header is incomplete".into());
    };
    if !version {
        return Err("Windows HTML clipboard header has no version".into());
    }
    let start = usize::try_from(start_fragment)
        .map_err(|_| "Windows HTML clipboard fragment starts at a negative offset")?;
    let end = usize::try_from(end_fragment)
        .map_err(|_| "Windows HTML clipboard fragment ends at a negative offset")?;
    if start < header_end || start > end || end > bytes.len() {
        return Err("Windows HTML clipboard fragment offsets are out of bounds".into());
    }
    if (start_html, end_html) != (-1, -1) {
        let context_start = usize::try_from(start_html)
            .map_err(|_| "Windows HTML clipboard context starts at a negative offset")?;
        let context_end = usize::try_from(end_html)
            .map_err(|_| "Windows HTML clipboard context ends at a negative offset")?;
        if context_start < header_end
            || context_start > start
            || context_end < end
            || context_end > bytes.len()
        {
            return Err("Windows HTML clipboard context offsets are out of bounds".into());
        }
    }
    let fragment = std::str::from_utf8(&bytes[start..end])
        .map_err(|_| "Windows HTML clipboard fragment is invalid UTF-8")?;
    let mut html = String::new();
    html.try_reserve_exact(fragment.len())
        .map_err(|_| "Windows HTML clipboard fragment allocation failed")?;
    html.push_str(fragment);
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_interoperates_with_crlf_and_allocation_padding() {
        let mut native = encode_unicode("café 日本語 👨‍👩‍👧‍👦\nsecond\r\nthird").unwrap();
        native.extend_from_slice(&[0; 8]);
        assert_eq!(
            decode_unicode(&native).unwrap(),
            "café 日本語 👨‍👩‍👧‍👦\nsecond\nthird"
        );
        assert_eq!(encode_unicode("").unwrap(), [0, 0]);
        assert!(encode_unicode("before\0after").is_err());
    }

    #[test]
    fn unicode_rejects_truncation_and_invalid_surrogates() {
        assert!(decode_unicode(&[b'a', 0]).is_err());
        assert!(decode_unicode(&[b'a', 0, 0]).is_err());
        assert!(decode_unicode(&[0, 0xd8, 0, 0]).is_err());
    }

    #[test]
    fn html_offsets_count_utf8_bytes_and_ignore_native_padding() {
        let fragment = "<p>日本語 <strong>café 👨‍👩‍👧‍👦</strong></p>";
        let mut native = encode_html(fragment).unwrap();
        native.extend_from_slice(&[0; 8]);
        assert_eq!(decode_html(&native).unwrap(), fragment);
        assert_eq!(decode_html(&encode_html("").unwrap()).unwrap(), "");
    }

    #[test]
    fn html_rejects_bad_bounds_and_split_utf8() {
        let mut native = encode_html("é").unwrap();
        let text = std::str::from_utf8(&native).unwrap();
        let start_key = text.find("StartFragment:").unwrap() + "StartFragment:".len();
        native[start_key..start_key + 10].copy_from_slice(b"9999999999");
        assert!(decode_html(&native).is_err());
        let mut native = encode_html("é").unwrap();
        let end_key = std::str::from_utf8(&native)
            .unwrap()
            .find("EndFragment:")
            .unwrap()
            + "EndFragment:".len();
        let split_end = HTML_HEADER.len() + HTML_PREFIX.len() + 1;
        native[end_key..end_key + 10].copy_from_slice(format!("{split_end:010}").as_bytes());
        assert!(decode_html(&native).is_err());
    }

    #[test]
    fn native_size_limit_is_checked_without_allocation() {
        assert!(checked_size(MAX_NATIVE_BYTES, "test").is_ok());
        assert!(checked_size(MAX_NATIVE_BYTES + 1, "test").is_err());
        assert!(byte_buffer(usize::MAX, "test").is_err());
    }
}
