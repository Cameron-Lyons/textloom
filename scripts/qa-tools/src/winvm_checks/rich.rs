//! Native editing and an independent Windows HTML clipboard consumer.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::{Caret, Checks, Rect, Session, all_true, field, listed, pretty, provenance, sleep};
use crate::native_report::{self, Clipboard, Expected, Mode, Os};
use crate::{Result, ResultExt};

const REPORT: &str = "C:\\textloom-qa\\rich-native.json";

fn begin(session: &mut Session, theme: Option<&str>) -> Result<()> {
    session.guest(&format!(
        "Remove-Item '{REPORT}' -ErrorAction SilentlyContinue; Out-Json @{{}}"
    ))?;
    let mut arguments = vec!["--rich-clipboard", "--qa-report", REPORT];
    if let Some(theme) = theme {
        arguments.extend(["--theme", theme]);
    }
    session.start(&arguments)?;
    session.focus_document()
}

fn finish(session: &mut Session, mut expectations: Value, observed: Value) -> Result<Value> {
    session.guest("Stop-ClipboardPeer; Out-Json @{}")?;
    session.focus_document()?;
    session.keys(&["ctrl+q"])?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.running()? && Instant::now() < deadline {
        sleep(0.3);
    }
    if session.running()? {
        return Err("rich editing host did not close gracefully".into());
    }
    session.guest(&format!(
        "Send-HostFile '{REPORT}' rich-native.json; Out-Json @{{}}"
    ))?;
    let path = session.output.join("native-report.json");
    fs::copy(session.checks.vm.inbox().join("rich-native.json"), &path)?;
    let expected = Expected {
        revision: provenance::revision(session.checks)?,
        os: Os::Windows,
        mode: Mode::Editable,
        interaction: true,
        clipboard: Clipboard::Rich,
    };
    let validation = native_report::check_file(&path, &expected);
    expectations["graceful native report validates with zero errors"] = validation.is_ok().into();
    Ok(
        json!({"passed": all_true(&expectations), "expectations": expectations,
        "observed": observed, "native_report": path,
        "report_validation": validation.err().map(|error| error.to_string())}),
    )
}

fn clipboard(session: &Session) -> Result<Value> {
    session.guest("Out-Json (Get-NativeClipboard)")
}

fn copy(session: &mut Session) -> Result<Value> {
    let sequence = clipboard(session)?["sequence"].clone();
    session.keys(&["ctrl+c"])?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let mut value = clipboard(session)?;
        if value["sequence"] != sequence {
            value["host_status"] = session.status().unwrap_or(Value::Null);
            return Ok(value);
        }
        if Instant::now() > deadline {
            return Err("copy did not publish a fresh clipboard offer".into());
        }
        sleep(0.2);
    }
}

fn same_rich(left: &Value, right: &Value) -> bool {
    left["tlfr"].is_string() && left["tlfr"] == right["tlfr"]
}

fn html(value: &Value) -> &str {
    value["fragment"].as_str().unwrap_or_default()
}

fn attributes(session: &Session) -> Result<Value> {
    session.guest("Out-Json (Get-SelectionAttributes)")
}

fn palette(session: &mut Session, target: &str, color: &str) -> Result<()> {
    session.click_element(&format!("{target} Color menu"), None, "Button")?;
    session.click_element(color, None, "Button")
}

fn kind(session: &mut Session, value: &str) -> Result<()> {
    session.click_element("", None, "ComboBox")?;
    session.click_element(value, None, "Button")
}

fn replace_document(session: &mut Session, text: &str) -> Result<()> {
    session.keys(&["ctrl+a"])?;
    session.click_element("Clear formatting", None, "Button")?;
    session.keys(&["ctrl+0"])?;
    session.type_text(text)?;
    if session.wait(|state| state.text == text)?.text != text {
        return Err("native fixture typing did not replace the document".into());
    }
    Ok(())
}

pub(super) fn colors(checks: &Checks) -> Result<Value> {
    checks.session("rich-colors", |session| {
        let snapshot_check = session.guest("Out-Json (Test-NativeClipboardSnapshot)")?;
        begin(session, None)?;
        session.select("format", None)?;
        let original = copy(session)?;
        session.click_element("Fill Color", None, "Button")?;
        palette(session, "Font", "Blue")?;
        let colored = copy(session)?;
        let colored_uia = attributes(session)?;
        session.screenshot("yellow-fill-blue-font")?;
        palette(session, "Fill", "Light Blue")?;
        let changed = copy(session)?;
        session.keys(&["ctrl+z"])?;
        let undone = copy(session)?;
        session.keys(&["ctrl+y"])?;
        let redone = copy(session)?;
        palette(session, "Fill", "No Fill")?;
        let no_fill = copy(session)?;
        palette(session, "Font", "Automatic")?;
        let automatic = copy(session)?;
        let expectations = json!({
            "clipboard capture keeps bytes and sequence paired across replacement": snapshot_check["passed"] == true,
            "native copy publishes Unicode, CF_HTML and TLFR": colored["formats"].as_array().is_some_and(|formats| formats.len() == 3) && colored["text"] == "format" && colored["tlfr"].is_string(),
            "font and fill export exact RGB values": html(&colored).contains("color:#0070c0;background-color:#ffff00"),
            "UIA exposes selected foreground and background": colored_uia["ForegroundColor"] == 0xc07000 && colored_uia["BackgroundColor"] == 0x00ffff,
            "choosing fill preserves font": html(&changed).contains("color:#0070c0;background-color:#00b0f0"),
            "one undo restores both colors losslessly": same_rich(&undone, &colored),
            "redo restores the new fill losslessly": same_rich(&redone, &changed),
            "No Fill removes only the background": !html(&no_fill).contains("background-color:") && html(&no_fill).contains("color:#0070c0"),
            "Automatic restores uncolored selection": same_rich(&automatic, &original),
        });
        finish(session, expectations, json!({"snapshot_consistency": snapshot_check, "colored": colored, "UIA": colored_uia, "changed": changed, "no_fill": no_fill, "automatic": automatic}))
    })
}

pub(super) fn soft_breaks(checks: &Checks) -> Result<Value> {
    checks.session("soft-breaks", |session| {
        begin(session, None)?;
        replace_document(session, "alpha")?;
        session.keys(&["shift+ret"])?;
        session.type_text("beta")?;
        let soft = session.state()?;
        let soft_kinds = session.kinds()?;
        session.keys(&["ctrl+a"])?;
        let copied = copy(session)?;
        session.keys(&["ctrl+end", "ctrl+shift+7", "ret"])?;
        session.type_text("gamma")?;
        let list_state = session.state()?;
        session.keys(&["ctrl+a"])?;
        let list = copy(session)?;
        session.screenshot("soft-break-in-list")?;
        // Rich native paste must retain the break as an inline separator.
        session.keys(&["backspace", "ctrl+v"])?;
        // Paste collapses selection; select all to compare native fragments.
        session.keys(&["ctrl+a"])?;
        let restored = copy(session)?;
        session.keys(&["ctrl+z"])?;
        let undone = session.state()?.text;
        session.keys(&["ctrl+y"])?;
        let redone = session.state()?.text;
        let expectations = json!({
            "Shift+Enter keeps one paragraph": soft_kinds.len() == 1 && soft.text.contains("alpha") && soft.text.contains("beta"),
            "plain clipboard maps the soft break to a newline": copied["text"] == "alpha\r\nbeta",
            "HTML uses br within one paragraph": html(&copied).contains("alpha<br>beta") && html(&copied).matches("<p>").count() == 1,
            "Enter continues the numbered list after a soft break": html(&list).contains("alpha<br>beta</li><li>gamma") && html(&list).matches("<li").count() == 2,
            "native rich paste preserves every TLFR byte": same_rich(&restored, &list),
            "one undo removes the whole rich paste": undone.is_empty(),
            "redo restores exact UIA text": redone == list_state.text,
        });
        finish(session, expectations, json!({"UIA_soft_text": soft.text, "soft_kinds": soft_kinds, "soft_clipboard": copied, "list_clipboard": list, "restored": restored}))
    })
}

pub(super) fn links(checks: &Checks) -> Result<Value> {
    checks.session("links", |session| {
        begin(session, None)?;
        session.select("format", None)?;
        let before = copy(session)?;
        session.keys(&["ctrl+k"])?;
        session.type_text("https://example.com/?a=1&b=2")?;
        session.keys(&["ret"])?;
        let linked = copy(session)?;
        let tree = session.guest("Out-Json (Get-Tree 8)")?;
        session.screenshot("link")?;
        session.keys(&["ctrl+z"])?;
        let undone = copy(session)?;
        session.keys(&["ctrl+y"])?;
        let redone = copy(session)?;
        session.click_element("Remove link", None, "Button")?;
        let removed = copy(session)?;
        let expectations = json!({
            "Ctrl+K links the selected text without changing it": linked["text"] == "format" && html(&linked).contains("<a href=\"https://example.com/?a=1&amp;b=2\">format</a>"),
            "UIA publishes a hyperlink": tree.to_string().contains("Hyperlink"),
            "undo removes the link losslessly": same_rich(&undone, &before),
            "redo restores the link losslessly": same_rich(&redone, &linked),
            "Remove link preserves text and other formatting": same_rich(&removed, &before),
        });
        fs::write(session.output.join("uia-tree.json"), pretty(&tree)?)?;
        finish(session, expectations, json!({"linked": linked, "removed": removed}))
    })
}

fn document_node(tree: &Value) -> Option<&Value> {
    if tree["type"] == "Edit" && tree["name"] == "Document" {
        return Some(tree);
    }
    tree["children"].as_array()?.iter().find_map(document_node)
}

fn column_mean(path: &Path, x: i64, y: i64, height: i64) -> Result<f64> {
    if x < 0 || y < 0 || height <= 0 {
        return Err("invalid quote rule screenshot region".into());
    }
    let output = Command::new("magick")
        .arg(path)
        .args([
            "-crop",
            &format!("1x{height}+{x}+{y}"),
            "+repage",
            "-format",
            "%[fx:mean]",
            "info:",
        ])
        .output()?;
    if !output.status.success() {
        return Err("quote rule pixel measurement failed".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("quote rule pixel mean")
}

pub(super) fn quotes(checks: &Checks) -> Result<Value> {
    let mut themes = Map::new();
    for theme in ["light", "dark"] {
        let value = checks.session(&format!("quotes-{theme}"), |session| {
            begin(session, Some(theme))?;
            replace_document(session, "first quote\nsecond quote\nbody")?;
            session.select("first quote\nsecond quote", None)?;
            kind(session, "Quote")?;
            let quoted = copy(session)?;
            let tree = session.guest("Out-Json (Get-Tree 8)")?;
            let kinds = session.kinds()?;
            let screenshot = session.screenshot("quoted")?;
            let document = document_node(&tree).ok_or("UIA tree lacks the document")?;
            let quote_group = &document["children"][0];
            let [x, y, _, height]: Rect = field(quote_group, "rect")?;
            let contrast = (column_mean(&screenshot, x + 1, y + 1, height - 2)?
                - column_mean(&screenshot, x - 3, y + 1, height - 2)?).abs();
            session.keys(&["ctrl+z"])?;
            let undone = copy(session)?;
            session.keys(&["ctrl+y"])?;
            let redone = copy(session)?;
            let expectations = json!({
                "two adjacent paragraphs export one blockquote": html(&quoted).matches("<blockquote>").count() == 1 && html(&quoted).contains("<blockquote><p>first quote</p><p>second quote</p></blockquote>"),
                "UIA groups two quote paragraphs separately from the body": kinds.len() == 2 && quote_group["children"].as_array().is_some_and(|children| children.len() == 2),
                "the quote rule is visibly distinct from the document background": contrast > 0.1,
                "undo removes the quote": undone["tlfr"].is_string() && !html(&undone).contains("blockquote"),
                "redo restores the quote losslessly": same_rich(&redone, &quoted),
                "the body paragraph is unchanged": session.state()?.text == "first quote\nsecond quote\nbody",
            });
            fs::write(session.output.join("uia-tree.json"), pretty(&tree)?)?;
            finish(session, expectations, json!({"theme": theme, "quoted": quoted, "kinds": kinds, "rule_pixel_contrast": contrast}))
        })?;
        themes.insert(theme.to_owned(), value);
    }
    Ok(json!({"passed": themes.values().all(|value| value["passed"] == true), "themes": themes}))
}

pub(super) fn numbering(checks: &Checks) -> Result<Value> {
    checks.session("numbering", |session| {
        begin(session, None)?;
        replace_document(session, "one\ntwo\nthree")?;
        session.keys(&["ctrl+a", "ctrl+shift+7"])?;
        let initial = copy(session)?;
        session.select("three", Some(Caret::End))?;
        let initial_status = session.status()?;
        session.click_element("Restart at 1", None, "Button")?;
        let restart_status = session.status()?;
        session.keys(&["ctrl+a"])?;
        let restarted = copy(session)?;
        session.select("three", Some(Caret::End))?;
        session.click_element("Continue numbering", None, "Button")?;
        let continue_status = session.status()?;
        session.keys(&["ctrl+a"])?;
        let continued = copy(session)?;
        session.select("two", Some(Caret::End))?;
        session.click_element("Indent", None, "Button")?;
        let indented_status = session.status()?;
        session.keys(&["ctrl+a"])?;
        let nested = copy(session)?;
        session.select("two", Some(Caret::End))?;
        session.click_element("Outdent", None, "Button")?;
        session.keys(&["ctrl+a"])?;
        let outdented = copy(session)?;
        session.select("three", None)?;
        let partial = copy(session)?;
        session.screenshot("numbered-list")?;
        let contains = |status: &Value, label: &str| listed(status.clone()).iter().any(|value| value == label);
        let expectations = json!({
            "three numbered items start at 1": html(&initial).contains("<ol start=\"1\">") && html(&initial).matches("<li").count() == 3 && contains(&initial_status, "Item 3."),
            "Restart at 1 changes the third marker": html(&restarted).contains("<li value=\"1\">three") && contains(&restart_status, "Item 1."),
            "Continue numbering restores the third marker": contains(&continue_status, "Item 3.") && same_rich(&continued, &initial),
            "Indent creates a nested list starting at 1": html(&nested).matches("<ol ").count() == 2 && contains(&indented_status, "Item 1."),
            "Outdent restores the original fragment": same_rich(&outdented, &initial),
            "copying only the third item preserves its computed start": html(&partial).contains("<ol start=\"3\"><li>three</li></ol>"),
        });
        finish(session, expectations, json!({"initial": initial, "restarted": restarted, "nested": nested, "partial": partial, "initial_status": initial_status, "restart_status": restart_status, "continue_status": continue_status, "indented_status": indented_status}))
    })
}

fn peer(session: &Session) -> Result<Value> {
    session.guest("Out-Json (Get-ClipboardPeer)")
}

fn focus_peer(session: &mut Session) -> Result<()> {
    let value = peer(session)?;
    let [window_x, window_y, width, height]: Rect = field(&value, "window")?;
    if width < 500 || height < 300 {
        return Err("independent clipboard peer is not visible".into());
    }
    // The peer extends beyond the host on the right. A real click there
    // raises it even when Windows denies a background process activation.
    session.monitor.click(
        window_x + width - 16,
        window_y + height / 2,
        "left",
        Some(session.size),
    )?;
    sleep(0.4);
    if peer(session)?["foreground"] != true {
        session.screenshot("peer-focus-failed")?;
        return Err("native click did not foreground the clipboard peer".into());
    }
    let [x, y]: [i64; 2] = field(&value, "origin")?;
    let [left, top, _, line_height]: Rect = field(&value["elements"][0], "rect")?;
    session.monitor.click(
        x + left + 10,
        y + top + line_height / 2,
        "left",
        Some(session.size),
    )?;
    sleep(0.5);
    Ok(())
}

pub(super) fn external_clipboard(checks: &Checks) -> Result<Value> {
    checks.session("external-rich-clipboard", |session| {
        begin(session, None)?;
        // Use the native fixture's Unicode, then apply every feature through UI.
        session.select("Unicode: café, 日本語, 👨\u{200d}👩\u{200d}👧\u{200d}👦.", None)?;
        session.keys(&["ctrl+b", "ctrl+i"])?;
        session.click_element("Fill Color", None, "Button")?;
        palette(session, "Font", "Blue")?;
        session.keys(&["ctrl+k"])?;
        session.type_text("https://example.com/clipboard")?;
        session.keys(&["ret"])?;
        kind(session, "Quote")?;
        session.select("café", Some(Caret::End))?;
        session.keys(&["shift+ret"])?;
        session.select("Lists, undo, search, and rich snapshots.\nEnter continues a list; Enter on an empty item exits it.", None)?;
        session.keys(&["ctrl+shift+7"])?;
        session.keys(&["ctrl+a"])?;
        let offered = copy(session)?;
        session.screenshot("textloom-offer")?;
        session.guest("Out-Json (Start-ClipboardPeer)")?;
        focus_peer(session)?;
        session.keys(&["ctrl+a", "ctrl+v"])?;
        sleep(1.0);
        let imported = peer(session)?;
        fs::write(session.output.join("mshtml-import.json"), pretty(&imported)?)?;
        session.screenshot("mshtml-import")?;
        let elements = imported["elements"].as_array().cloned().unwrap_or_default();
        let has = |tag: &str| elements.iter().any(|element| element["tag"] == tag);
        let styled = elements.iter().any(|element| {
            element["text"].as_str().is_some_and(|text| text.contains("café"))
                && element["color"] == "#0070c0" && element["background"] == "#ffff00"
        });
        // The peer authors and renders its own fixture, then its native copy
        // replaces the private TLFR offer. Textloom intentionally imports plain.
        session.guest("Set-Content -Encoding ASCII C:\\textloom-qa\\clipboard-peer-request.json '{\"action\":\"fixture\",\"generation\":1}'; Out-Json @{}")?;
        sleep(1.0);
        let external = peer(session)?;
        focus_peer(session)?;
        session.keys(&["ctrl+a"])?;
        let incoming = copy(session)?;
        session.screenshot("mshtml-external-fixture")?;
        session.guest("Stop-ClipboardPeer; Out-Json @{}")?;
        session.focus_document()?;
        replace_document(session, "Replace me")?;
        session.keys(&["ctrl+a"])?;
        let before = session.state()?.text;
        session.keys(&["ctrl+v"])?;
        let incoming_text = field::<String>(&incoming, "text")?.replace("\r\n", "\n");
        let pasted = session.wait(|state| state.text != before)?.text;
        session.keys(&["ctrl+a"])?;
        let fallback = copy(session)?;
        session.keys(&["ctrl+z"])?;
        let undone = session.state()?.text;
        session.keys(&["ctrl+y"])?;
        let redone = session.state()?.text;
        let expectations = json!({
            "MSHTML imports Unicode and the soft line break": imported["text"].as_str().is_some_and(|text| text.contains("café\r\n, 日本語, 👨\u{200d}👩\u{200d}👧\u{200d}👦.")),
            "MSHTML independently retains font and fill RGB": styled,
            "MSHTML retains bold and italic": has("STRONG") && has("EM"),
            "MSHTML retains the link target": elements.iter().any(|element| element["tag"] == "A" && element["href"] == "https://example.com/clipboard"),
            "MSHTML retains a blockquote and ordered list": has("BLOCKQUOTE") && elements.iter().any(|element| element["tag"] == "OL" && element["start"] == "1") && elements.iter().filter(|element| element["tag"] == "LI").count() == 2,
            "external native copy provides HTML and Unicode without TLFR": incoming["fragment"].is_string() && incoming["text"].as_str().is_some_and(|text| text.contains("External café")) && incoming["tlfr"].is_null() && external["generation"] == 1,
            "external rich paste uses exact plain fallback": pasted == incoming_text,
            "fallback does not import external emphasis or list structure": fallback["tlfr"].is_string() && !html(&fallback).contains("<strong>") && !html(&fallback).contains("<em>") && !html(&fallback).contains("<ol"),
            "one undo restores the original document": undone == before,
            "redo restores exact external text": redone == pasted,
        });
        finish(session, expectations, json!({"offered": offered, "engine": imported["engine"], "incoming": incoming, "fallback": fallback, "pasted_UIA": pasted}))
    })
}

pub(super) fn plain_paste(checks: &Checks) -> Result<Value> {
    checks.session("plain-paste-shortcut", |session| {
        begin(session, None)?;
        session.select("format", None)?;
        session.keys(&["ctrl+b"])?;
        session.click_element("Fill Color", None, "Button")?;
        let rich = copy(session)?;
        session.select("writing", None)?;
        let before = session.state()?.text;
        session.keys(&["ctrl+shift+v"])?;
        let pasted = session.wait(|state| state.text != before)?.text;
        session.select_in("format", 2, None, "Document")?;
        let plain = copy(session)?;
        session.keys(&["ctrl+z"])?;
        let undone = session.state()?.text;
        session.keys(&["ctrl+y"])?;
        let redone = session.state()?.text;
        session.screenshot("plain-paste")?;
        let expectations = json!({
            "the original offer is rich": html(&rich).contains("<strong>") && html(&rich).contains("background-color:#ffff00"),
            "Ctrl+Shift+V replaces exactly once": pasted == before.replacen("writing", "format", 1),
            "plain paste drops the source styles": plain["tlfr"].is_string() && plain["text"] == "format" && !html(&plain).contains("<strong>") && !html(&plain).contains("background-color:"),
            "one undo restores the replacement": undone == before,
            "redo restores exact plain text": redone == pasted,
        });
        finish(session, expectations, json!({"rich_offer": rich, "plain_result": plain, "pasted_UIA": pasted}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_rich_formats_never_count_as_lossless_preservation() {
        assert!(!same_rich(&json!({"tlfr": null}), &json!({"tlfr": null})));
        assert!(!same_rich(&json!({}), &json!({})));
        assert!(!same_rich(&json!({"tlfr": "a"}), &json!({"tlfr": "b"})));
        assert!(same_rich(&json!({"tlfr": "a"}), &json!({"tlfr": "a"})));
    }
}
