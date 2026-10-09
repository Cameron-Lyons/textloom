//! Linux clipboard operations run only inside the host's managed helper process.
//!
//! The parent supplies a whole-operation deadline, kills and reaps stalled helpers,
//! and verifies all representations after the provisional `READY` response. The
//! Wayland data-control transport is optional; ordinary window clipboard protocols
//! remain owned by eframe. This module never starts a serving thread.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::OFlags;
use wl_clipboard_rs::{copy, paste, watch};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux,
    EventMask, GetPropertyReply, PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{MAX_NATIVE_BYTES, MIME, NativeItem};

const HTML_MIME: &str = "text/html";
const PLAIN_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_INCR_TRANSFERS: usize = 16;
const CHUNK_BYTES: usize = 64 * 1024;

/// Publish, acknowledge provisional readiness, and serve until another owner takes
/// over. Only the parent may interpret native readback as successful publication.
pub(super) fn publish(item: &NativeItem, _owner: usize) -> Result<(), String> {
    validate_item(item)?;
    if use_wayland() {
        let mut options = copy::Options::new();
        options.foreground(true);
        let sources = vec![
            // wl-clipboard-rs uses the first text/* source for additional plain
            // aliases. Keep actual plain text before HTML, which also qualifies.
            copy::MimeSource {
                source: copy::Source::Bytes(item.plain.as_bytes().into()),
                mime_type: copy::MimeType::Text,
            },
            copy::MimeSource {
                source: copy::Source::Bytes(item.rich.clone().into()),
                mime_type: copy::MimeType::Specific(MIME.into()),
            },
            copy::MimeSource {
                source: copy::Source::Bytes(item.html.as_bytes().into()),
                mime_type: copy::MimeType::Specific(HTML_MIME.into()),
            },
        ];
        match options.prepare_copy_multi(sources) {
            Ok(prepared) => {
                ready()?;
                // The library queues publication during preparation and flushes
                // while serving. Its receiver writes may block; the parent owns
                // this entire process and can always retire it on replacement.
                return prepared
                    .serve()
                    .map_err(|_| "Wayland clipboard serving failed".into());
            }
            Err(copy::Error::MissingProtocol { .. }) if has_x11() => {}
            Err(_) => return Err("Wayland clipboard publication unavailable".into()),
        }
    }
    publish_x11(item)
}

/// Capture the native representations from one Wayland offer or one checked X11
/// selection owner. Missing rich data returns None before reading alternatives;
/// missing HTML or plain representations are empty.
pub(super) fn read() -> Result<Option<NativeItem>, String> {
    if use_wayland() {
        match watch::Watcher::new(watch::ClipboardType::Regular, paste::Seat::Unspecified) {
            Ok(mut watcher) => {
                let deadline = Instant::now() + TRANSFER_TIMEOUT;
                return match watcher
                    .next_event()
                    .map_err(|_| "Wayland clipboard offer failed")?
                {
                    Some(watch::ClipboardEvent::Changed {
                        mime_types,
                        mut offer,
                        ..
                    }) => {
                        let rich = read_wayland_type(&mut offer, &mime_types, MIME, deadline)?;
                        if rich.is_empty() {
                            return Ok(None);
                        }
                        let html = read_wayland_type(&mut offer, &mime_types, HTML_MIME, deadline)?;
                        let plain_type = PLAIN_MIMES
                            .iter()
                            .find(|mime| mime_types.iter().any(|s| s == **mime));
                        let plain = match plain_type {
                            Some(mime) => {
                                read_wayland_type(&mut offer, &mime_types, mime, deadline)?
                            }
                            None => Vec::new(),
                        };
                        Ok(Some(make_item(plain, html, rich)?))
                    }
                    Some(watch::ClipboardEvent::Cleared { .. }) | None => Ok(None),
                };
            }
            Err(paste::Error::MissingProtocol { .. }) if has_x11() => {}
            Err(_) => return Err("Wayland clipboard acquisition unavailable".into()),
        }
    }
    read_x11()
}

fn use_wayland() -> bool {
    match std::env::var("TEXTLOOM_CLIPBOARD_BACKEND").as_deref() {
        Ok("x11") => false,
        Ok("wayland") => true,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_some(),
    }
}

fn has_x11() -> bool {
    std::env::var_os("DISPLAY").is_some()
}

fn ready() -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(b"READY\n")
        .and_then(|()| stdout.flush())
        .map_err(|_| "Clipboard readiness response failed".into())
}

fn validate_item(item: &NativeItem) -> Result<(), String> {
    if [item.plain.len(), item.html.len(), item.rich.len()]
        .into_iter()
        .any(|size| size > MAX_NATIVE_BYTES)
    {
        return Err("Clipboard representation exceeds the byte limit".into());
    }
    Ok(())
}

fn make_item(plain: Vec<u8>, html: Vec<u8>, rich: Vec<u8>) -> Result<NativeItem, String> {
    Ok(NativeItem {
        plain: String::from_utf8(plain).map_err(|_| "Clipboard plain text is not UTF-8")?,
        html: String::from_utf8(html).map_err(|_| "Clipboard HTML is not UTF-8")?,
        rich,
    })
}

fn read_wayland_type(
    offer: &mut watch::Offer<'_>,
    types: &[String],
    mime: &str,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    if !types.iter().any(|offered| offered == mime) {
        return Ok(Vec::new());
    }
    let pipe = offer
        .receive(mime)
        .map_err(|_| "Wayland clipboard transfer failed")?;
    read_pipe(pipe, MAX_NATIVE_BYTES, deadline)
}

fn wait_fd(fd: &impl AsFd, deadline: Instant) -> Result<(), String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("Clipboard transfer timed out")?;
        let timeout = Timespec::try_from(remaining).map_err(|_| "Clipboard deadline invalid")?;
        let mut fds = [PollFd::new(fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err("Clipboard transfer timed out".into()),
            Ok(_) if fds[0].revents().contains(PollFlags::NVAL) => {
                return Err("Clipboard transfer descriptor unavailable".into());
            }
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err("Clipboard transfer polling failed".into()),
        }
    }
}

fn read_pipe(
    mut pipe: impl Read + AsFd,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let flags = rustix::fs::fcntl_getfl(&pipe).map_err(|_| "Clipboard pipe flags unavailable")?;
    rustix::fs::fcntl_setfl(&pipe, flags | OFlags::NONBLOCK)
        .map_err(|_| "Clipboard pipe cannot be bounded")?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err("Clipboard transfer timed out".into());
        }
        // Read one byte beyond the limit to distinguish exact-size EOF from an
        // oversized stream without allowing an unbounded allocation.
        let count = chunk
            .len()
            .min(limit.saturating_sub(bytes.len()).saturating_add(1));
        match pipe.read(&mut chunk[..count]) {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                if count > limit.saturating_sub(bytes.len()) {
                    return Err("Clipboard representation exceeds the byte limit".into());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => wait_fd(&pipe, deadline)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Clipboard pipe read failed".into()),
        }
    }
}

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    timestamp: Atom,
    incr: Atom,
    property: Atom,
    rich: Atom,
    html: Atom,
    plain: [Atom; 3],
}

impl Atoms {
    fn new(connection: &RustConnection) -> Result<Self, String> {
        let atom = |name: &str| -> Result<Atom, String> {
            connection
                .intern_atom(false, name.as_bytes())
                .map_err(|_| "X11 clipboard atom request failed")?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|_| "X11 clipboard atom unavailable".into())
        };
        Ok(Self {
            clipboard: atom("CLIPBOARD")?,
            targets: atom("TARGETS")?,
            timestamp: atom("TIMESTAMP")?,
            incr: atom("INCR")?,
            property: atom("TEXTLOOM_CLIPBOARD_TRANSFER")?,
            rich: atom(MIME)?,
            html: atom(HTML_MIME)?,
            plain: [
                atom("UTF8_STRING")?,
                atom(PLAIN_MIMES[0])?,
                atom(PLAIN_MIMES[1])?,
            ],
        })
    }
}

fn x11_context() -> Result<(RustConnection, Window, Atoms), String> {
    let (connection, screen) =
        x11rb::connect(None).map_err(|_| "X11 clipboard connection failed")?;
    let window = connection
        .generate_id()
        .map_err(|_| "X11 clipboard window unavailable")?;
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            connection.setup().roots[screen].root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|_| "X11 clipboard window creation failed")?
        .check()
        .map_err(|_| "X11 clipboard window creation failed")?;
    let atoms = Atoms::new(&connection)?;
    Ok((connection, window, atoms))
}

fn next_x11_event(connection: &RustConnection, deadline: Instant) -> Result<Event, String> {
    loop {
        if Instant::now() >= deadline {
            return Err("Clipboard transfer timed out".into());
        }
        if let Some(event) = connection
            .poll_for_event()
            .map_err(|_| "X11 clipboard event failed")?
        {
            return Ok(event);
        }
        wait_fd(connection.stream(), deadline)?;
    }
}

fn selection_owner(connection: &RustConnection, atoms: &Atoms) -> Result<Window, String> {
    connection
        .get_selection_owner(atoms.clipboard)
        .map_err(|_| "X11 selection owner request failed")?
        .reply()
        .map(|reply| reply.owner)
        .map_err(|_| "X11 selection owner unavailable".into())
}

fn server_time(connection: &RustConnection, window: Window, atoms: &Atoms) -> Result<u32, String> {
    connection
        .change_property8(
            PropMode::REPLACE,
            window,
            atoms.property,
            AtomEnum::INTEGER,
            &[0],
        )
        .map_err(|_| "X11 timestamp request failed")?
        .check()
        .map_err(|_| "X11 timestamp request failed")?;
    connection
        .flush()
        .map_err(|_| "X11 clipboard flush failed")?;
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    loop {
        if let Event::PropertyNotify(event) = next_x11_event(connection, deadline)?
            && event.window == window
            && event.atom == atoms.property
            && event.state == Property::NEW_VALUE
        {
            return Ok(event.time);
        }
    }
}

struct Outgoing {
    window: Window,
    property: Atom,
    target: Atom,
    bytes: Arc<[u8]>,
    offset: usize,
    deadline: Instant,
}

fn publish_x11(item: &NativeItem) -> Result<(), String> {
    let (connection, window, atoms) = x11_context()?;
    let timestamp = server_time(&connection, window, &atoms)?;
    connection
        .set_selection_owner(window, atoms.clipboard, timestamp)
        .map_err(|_| "X11 clipboard publication failed")?
        .check()
        .map_err(|_| "X11 clipboard publication failed")?;
    if selection_owner(&connection, &atoms)? != window {
        return Err("X11 clipboard ownership unavailable".into());
    }
    let plain: Arc<[u8]> = Arc::from(item.plain.as_bytes());
    let mut data = HashMap::from([
        (atoms.rich, Arc::from(item.rich.as_slice())),
        (atoms.html, Arc::from(item.html.as_bytes())),
    ]);
    for target in atoms.plain {
        data.insert(target, Arc::clone(&plain));
    }
    let mut targets: Vec<Atom> = data.keys().copied().collect();
    targets.extend([atoms.targets, atoms.timestamp]);
    let chunk_size = CHUNK_BYTES
        .min(connection.maximum_request_bytes().saturating_sub(64))
        .max(1);
    let mut outgoing: Vec<Outgoing> = Vec::new();
    ready()?;
    loop {
        outgoing.retain(|transfer| transfer.deadline > Instant::now());
        let event = match next_x11_event(&connection, Instant::now() + Duration::from_secs(1)) {
            Ok(event) => event,
            Err(error) if error == "Clipboard transfer timed out" => continue,
            Err(error) => return Err(error),
        };
        match event {
            Event::SelectionClear(event) if event.selection == atoms.clipboard => return Ok(()),
            Event::SelectionRequest(event) if event.selection == atoms.clipboard => {
                serve_request(
                    &connection,
                    &atoms,
                    &event,
                    timestamp,
                    &targets,
                    &data,
                    chunk_size,
                    &mut outgoing,
                );
            }
            Event::PropertyNotify(event) if event.state == Property::DELETE => {
                if let Some(index) = outgoing.iter().position(|transfer| {
                    transfer.window == event.window && transfer.property == event.atom
                }) {
                    let transfer = &mut outgoing[index];
                    let end = (transfer.offset + chunk_size).min(transfer.bytes.len());
                    let result = connection.change_property8(
                        PropMode::REPLACE,
                        transfer.window,
                        transfer.property,
                        transfer.target,
                        &transfer.bytes[transfer.offset..end],
                    );
                    if !result.is_ok_and(|cookie| cookie.check().is_ok()) || end == transfer.offset
                    {
                        outgoing.swap_remove(index);
                    } else {
                        transfer.offset = end;
                    }
                }
            }
            _ => {}
        }
        connection
            .flush()
            .map_err(|_| "X11 clipboard flush failed")?;
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_request(
    connection: &RustConnection,
    atoms: &Atoms,
    event: &SelectionRequestEvent,
    timestamp: u32,
    targets: &[Atom],
    data: &HashMap<Atom, Arc<[u8]>>,
    chunk_size: usize,
    outgoing: &mut Vec<Outgoing>,
) {
    let property = if event.property == x11rb::NONE {
        event.target
    } else {
        event.property
    };
    // Refuse overlapping transfers to the same property rather than mixing them.
    let result = if outgoing
        .iter()
        .any(|transfer| transfer.window == event.requestor && transfer.property == property)
    {
        false
    } else if event.target == atoms.targets {
        connection
            .change_property32(
                PropMode::REPLACE,
                event.requestor,
                property,
                AtomEnum::ATOM,
                targets,
            )
            .is_ok_and(|cookie| cookie.check().is_ok())
    } else if event.target == atoms.timestamp {
        connection
            .change_property32(
                PropMode::REPLACE,
                event.requestor,
                property,
                AtomEnum::INTEGER,
                &[timestamp],
            )
            .is_ok_and(|cookie| cookie.check().is_ok())
    } else if let Some(bytes) = data.get(&event.target) {
        if bytes.len() <= chunk_size {
            connection
                .change_property8(
                    PropMode::REPLACE,
                    event.requestor,
                    property,
                    event.target,
                    bytes,
                )
                .is_ok_and(|cookie| cookie.check().is_ok())
        } else if outgoing.len() < MAX_INCR_TRANSFERS {
            let selected = connection
                .change_window_attributes(
                    event.requestor,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                )
                .is_ok_and(|cookie| cookie.check().is_ok());
            let written = selected
                && connection
                    .change_property32(
                        PropMode::REPLACE,
                        event.requestor,
                        property,
                        atoms.incr,
                        &[bytes.len() as u32],
                    )
                    .is_ok_and(|cookie| cookie.check().is_ok());
            if written {
                outgoing.push(Outgoing {
                    window: event.requestor,
                    property,
                    target: event.target,
                    bytes: Arc::clone(bytes),
                    offset: 0,
                    deadline: Instant::now() + TRANSFER_TIMEOUT,
                });
            }
            written
        } else {
            false
        }
    } else {
        false
    };
    let reply = SelectionNotifyEvent {
        response_type: xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: event.time,
        requestor: event.requestor,
        selection: event.selection,
        target: event.target,
        property: if result { property } else { x11rb::NONE },
    };
    // A requestor may disappear at any point. Its failure must not terminate the
    // clipboard owner or expose native error strings containing clipboard data.
    if let Ok(cookie) = connection.send_event(false, event.requestor, EventMask::NO_EVENT, reply) {
        let _ = cookie.check();
    }
}

fn bounded_property(
    connection: &RustConnection,
    window: Window,
    property: Atom,
    limit: usize,
) -> Result<GetPropertyReply, String> {
    let words = u32::try_from(limit.div_ceil(4)).map_err(|_| "Clipboard byte limit invalid")?;
    let reply = connection
        .get_property(true, window, property, AtomEnum::ANY, 0, words)
        .map_err(|_| "X11 clipboard property request failed")?
        .reply()
        .map_err(|_| "X11 clipboard property read failed")?;
    if reply.bytes_after != 0 || reply.value.len() > limit {
        return Err("Clipboard representation exceeds the byte limit".into());
    }
    Ok(reply)
}

fn read_target(
    connection: &RustConnection,
    window: Window,
    atoms: &Atoms,
    target: Atom,
    deadline: Instant,
) -> Result<Option<GetPropertyReply>, String> {
    connection
        .delete_property(window, atoms.property)
        .map_err(|_| "X11 clipboard reset failed")?
        .check()
        .map_err(|_| "X11 clipboard reset failed")?;
    connection
        .convert_selection(
            window,
            atoms.clipboard,
            target,
            atoms.property,
            x11rb::CURRENT_TIME,
        )
        .map_err(|_| "X11 clipboard conversion failed")?
        .check()
        .map_err(|_| "X11 clipboard conversion failed")?;
    connection
        .flush()
        .map_err(|_| "X11 clipboard flush failed")?;
    loop {
        if let Event::SelectionNotify(event) = next_x11_event(connection, deadline)?
            && event.requestor == window
            && event.selection == atoms.clipboard
            && event.target == target
        {
            if event.property == x11rb::NONE {
                return Ok(None);
            }
            if event.property != atoms.property {
                return Err("X11 clipboard response invalid".into());
            }
            break;
        }
    }
    let limit = if target == atoms.targets {
        4096
    } else if target == atoms.timestamp {
        4
    } else {
        MAX_NATIVE_BYTES
    };
    let mut reply = bounded_property(connection, window, atoms.property, limit)?;
    if reply.type_ != atoms.incr {
        return Ok(Some(reply));
    }
    let length = reply
        .value32()
        .and_then(|mut words| words.next())
        .ok_or("X11 incremental clipboard header invalid")? as usize;
    if reply.format != 32 || length > limit {
        return Err("Clipboard representation exceeds the byte limit".into());
    }
    let mut bytes = Vec::new();
    let mut kind = None;
    connection
        .flush()
        .map_err(|_| "X11 clipboard flush failed")?;
    loop {
        // The initial INCR property's NEW_VALUE event may still be queued. Reading
        // a deleted property yields type NONE; skip it until an actual chunk arrives.
        if let Event::PropertyNotify(event) = next_x11_event(connection, deadline)?
            && event.window == window
            && event.atom == atoms.property
            && event.state == Property::NEW_VALUE
        {
            let chunk = bounded_property(
                connection,
                window,
                atoms.property,
                limit.saturating_sub(bytes.len()),
            )?;
            if chunk.type_ == x11rb::NONE {
                continue;
            }
            if chunk.format != 8 || kind.is_some_and(|kind| kind != chunk.type_) {
                return Err("X11 incremental clipboard chunk invalid".into());
            }
            kind = Some(chunk.type_);
            if chunk.value.is_empty() {
                reply.type_ = chunk.type_;
                reply.format = 8;
                reply.value_len = bytes.len() as u32;
                reply.value = bytes;
                reply.bytes_after = 0;
                return Ok(Some(reply));
            }
            bytes.extend_from_slice(&chunk.value);
            connection
                .flush()
                .map_err(|_| "X11 clipboard flush failed")?;
        }
    }
}

fn read_timestamp(
    connection: &RustConnection,
    window: Window,
    atoms: &Atoms,
    deadline: Instant,
) -> Result<Option<u32>, String> {
    Ok(
        read_target(connection, window, atoms, atoms.timestamp, deadline)?.and_then(|reply| {
            if reply.type_ == AtomEnum::INTEGER.into() {
                reply.value32().and_then(|mut values| values.next())
            } else {
                None
            }
        }),
    )
}

fn read_x11() -> Result<Option<NativeItem>, String> {
    let (connection, window, atoms) = x11_context()?;
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    let owner = selection_owner(&connection, &atoms)?;
    if owner == x11rb::NONE {
        return Ok(None);
    }
    let timestamp = read_timestamp(&connection, window, &atoms, deadline)?;
    let targets = read_target(&connection, window, &atoms, atoms.targets, deadline)?
        .and_then(|reply| reply.value32().map(|values| values.collect::<Vec<_>>()));
    let get = |target| -> Result<Vec<u8>, String> {
        if targets
            .as_ref()
            .is_some_and(|targets| !targets.contains(&target))
        {
            return Ok(Vec::new());
        }
        if selection_owner(&connection, &atoms)? != owner {
            return Err("X11 clipboard changed during acquisition".into());
        }
        match read_target(&connection, window, &atoms, target, deadline)? {
            Some(reply) if reply.format == 8 => Ok(reply.value),
            Some(_) => Err("X11 clipboard representation format invalid".into()),
            None => Ok(Vec::new()),
        }
    };
    let rich = get(atoms.rich)?;
    if rich.is_empty() {
        return Ok(None);
    }
    let html = get(atoms.html)?;
    let mut plain = Vec::new();
    for target in atoms.plain {
        plain = get(target)?;
        if !plain.is_empty() {
            break;
        }
    }
    let final_timestamp = read_timestamp(&connection, window, &atoms, deadline)?;
    if final_timestamp != timestamp || selection_owner(&connection, &atoms)? != owner {
        return Err("X11 clipboard changed during acquisition".into());
    }
    Ok(Some(make_item(plain, html, rich)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn pipe_pair() -> (File, File) {
        let (reader, writer) = rustix::pipe::pipe().unwrap();
        (File::from(reader), File::from(writer))
    }

    #[test]
    fn pipe_accepts_exact_limit_with_utf8_bytes() {
        let (reader, mut writer) = pipe_pair();
        writer.write_all("日本é".as_bytes()).unwrap();
        drop(writer);
        assert_eq!(
            read_pipe(reader, 8, Instant::now() + TRANSFER_TIMEOUT).unwrap(),
            "日本é".as_bytes()
        );
    }

    #[test]
    fn pipe_rejects_over_limit_and_empty_limit_data() {
        for limit in [0, 3] {
            let (reader, mut writer) = pipe_pair();
            writer.write_all(b"four").unwrap();
            drop(writer);
            assert_eq!(
                read_pipe(reader, limit, Instant::now() + TRANSFER_TIMEOUT).unwrap_err(),
                "Clipboard representation exceeds the byte limit"
            );
        }
    }

    #[test]
    fn pipe_deadline_interrupts_a_nonclosing_sender() {
        let (reader, _writer) = pipe_pair();
        let start = Instant::now();
        assert_eq!(
            read_pipe(reader, 100, start + Duration::from_millis(20)).unwrap_err(),
            "Clipboard transfer timed out"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn pipe_supports_empty_eof_and_rejects_expired_deadline() {
        let (reader, writer) = pipe_pair();
        drop(writer);
        assert!(
            read_pipe(reader, 0, Instant::now() + TRANSFER_TIMEOUT)
                .unwrap()
                .is_empty()
        );
        let (reader, _writer) = pipe_pair();
        assert_eq!(
            read_pipe(reader, 1, Instant::now()).unwrap_err(),
            "Clipboard transfer timed out"
        );
    }

    #[test]
    fn invalid_utf8_errors_never_include_native_contents() {
        let error = make_item(vec![0xff], b"secret".to_vec(), b"private".to_vec())
            .err()
            .unwrap();
        assert_eq!(error, "Clipboard plain text is not UTF-8");
    }
}
