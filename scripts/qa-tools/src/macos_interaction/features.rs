//! Native feature checks use public fixture text and independently inspected
//! clipboard HTML/RTF. Exact HTML comparisons catch lost styles and unwanted
//! extra paragraphs; they are not screenshot or screen-reader signoff.

use serde_json::{Value, json};

use super::{
    ClipboardCheck, CopyScope, END, Exit, Key, PASTE, Probe, START, System, TITLE, change_count,
    stem, strings,
};
use crate::Result;

#[derive(Clone, Copy)]
struct Rich<'a> {
    text: &'a str,
    body: &'a str,
}

const TEXT: &str = "café link\nsecond";
const PLAIN: Rich<'_> = Rich {
    text: TEXT,
    body: "<p>café link</p><p>second</p>",
};
const FILLED: Rich<'_> = Rich {
    text: TEXT,
    body: "<p><span style=\"background-color:#ffff00\">café link</span></p><p><span style=\"background-color:#ffff00\">second</span></p>",
};
const COLORED: Rich<'_> = Rich {
    text: TEXT,
    body: "<p><span style=\"color:#ff0000;background-color:#ffff00\">café link</span></p><p><span style=\"color:#ff0000;background-color:#ffff00\">second</span></p>",
};
const FONT_ONLY: Rich<'_> = Rich {
    text: TEXT,
    body: "<p><span style=\"color:#ff0000\">café link</span></p><p><span style=\"color:#ff0000\">second</span></p>",
};
const URL: &str = "https://example.invalid/textloom";

fn html_document(body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><div style=\"white-space:pre-wrap\">{body}</div></body></html>"
    )
}

fn check_html(observed: &Value, body: &str) -> Result<()> {
    if observed.as_str() != Some(&html_document(body)) {
        return Err(
            format!("Native HTML differs: expected body {body:?}, observed {observed}").into(),
        );
    }
    Ok(())
}

/// A native wrap must yield a nonempty, proper suffix of the single paragraph.
/// Measuring that suffix avoids encoding a particular font or line width.
fn wrapped_prefix<'a>(fixture: &'a str, suffix: &str) -> Result<&'a str> {
    if fixture.contains(['\n', '\r', '\u{2028}']) || suffix.is_empty() || suffix == fixture {
        return Err("Command-Shift-Left did not witness a forced visual wrap".into());
    }
    fixture
        .strip_suffix(suffix)
        .ok_or_else(|| "Wrapped selection is not an exact fixture suffix".into())
}

impl<S: System> Probe<S> {
    fn keys(&mut self, pid: u32, keys: &[Key]) -> Result<()> {
        for key in keys {
            self.action(pid, TITLE, key.command, key.value)?;
        }
        Ok(())
    }

    fn rich(&mut self, pid: u32, expected: Rich<'_>) -> Result<()> {
        self.copy_document(pid, TITLE, expected.text)?;
        let offer = self.clipboard("native-html", &[], None)?;
        self.observe(
            "last_feature_html",
            json!({"expected_body": expected.body, "observed": offer}),
        );
        self.save()?;
        check_html(
            &offer.get("html").cloned().unwrap_or(Value::Null),
            expected.body,
        )
    }

    fn feature_fixture(&mut self, pid: u32, expected: Rich<'_>) -> Result<()> {
        self.shortcut_fixture(pid, expected.text)?;
        self.keys(
            pid,
            &[
                Key::new("chord", "0"),
                Key::new("press-button", "Clear formatting"),
            ],
        )?;
        self.rich(pid, expected)
    }

    fn rich_change(
        &mut self,
        pid: u32,
        name: &str,
        before: Rich<'_>,
        after: Rich<'_>,
        keys: &[Key],
    ) -> Result<()> {
        self.keys(pid, keys)?;
        self.rich(pid, after)?;
        self.action(pid, TITLE, "chord", "z")?;
        self.rich(pid, before)?;
        self.action(pid, TITLE, "shift-chord", "z")?;
        self.rich(pid, after)?;
        self.note(
            name,
            json!({"plain_utf8_exact": true, "html_exact": true, "single_undo_redo": true}),
        )
    }

    fn feature_import(&mut self, name: &str, specification: Value) -> Result<()> {
        // The last rich assertion left the native offer on the clipboard.
        let pid = self.textedit_pid.ok_or("TextEdit is not running")?;
        let window = stem(&self.import_file);
        self.action(pid, &window, "chord", "a")?;
        self.action(pid, &window, "chord", "v")?;
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        self.action(pid, &window, "chord", "a")?;
        self.action(pid, &window, "chord", "c")?;
        self.diagnostic("feature-import")?;
        let imported = self.clipboard(
            "textedit-features",
            &[&specification.to_string()],
            Some(generation),
        )?;
        self.note(name, Value::Object(imported))?;
        self.action(pid, &window, "chord", "s")
    }

    pub(super) fn features(&mut self, pid: u32) -> Result<()> {
        self.feature_fixture(pid, PLAIN)?;
        self.rich_change(
            pid,
            "features: Fill Color and undo/redo",
            PLAIN,
            FILLED,
            &[Key::new("press-button", "Fill Color")],
        )?;
        self.rich_change(
            pid,
            "features: Font Color and undo/redo",
            FILLED,
            COLORED,
            &[Key::new("press-button", "Font Color")],
        )?;
        let linked_body = COLORED
            .body
            .replace("<p>", &format!("<p><a href=\"{URL}\">"))
            .replace("</p>", "</a></p>");
        let linked = Rich {
            text: TEXT,
            body: &linked_body,
        };
        self.rich_change(
            pid,
            "features: Command-K add link and undo/redo",
            COLORED,
            linked,
            &[
                Key::new("chord", "k"),
                Key::new("type", URL),
                Key::new("keycode", "36"),
            ],
        )?;

        self.plain_paste(pid, linked)?;

        // Re-create the offer through native controls, without depending on the
        // plain-paste clipboard that the preceding assertion published.
        self.feature_fixture(pid, PLAIN)?;
        self.keys(
            pid,
            &[
                Key::new("press-button", "Fill Color"),
                Key::new("press-button", "Font Color"),
                Key::new("press-button", "Link"),
                Key::new("type", URL),
                Key::new("keycode", "36"),
            ],
        )?;
        self.rich(pid, linked)?;
        self.feature_import(
            "features: TextEdit preserves fill, font color, and hyperlink",
            json!({
                "paragraphs": [{"text": "café link"}, {"text": "second"}],
                "foreground": [255,0,0,255], "background": [255,255,0,255], "link": URL
            }),
        )?;
        let edited_body = linked_body.replace(URL, "https://example.invalid/edited");
        let edited = Rich {
            text: TEXT,
            body: &edited_body,
        };
        self.rich_change(
            pid,
            "features: Link button edits target and undo/redo",
            linked,
            edited,
            &[
                Key::new("press-button", "Link"),
                Key::new("chord", "a"),
                Key::new("type", "https://example.invalid/edited"),
                Key::new("keycode", "36"),
            ],
        )?;
        self.rich_change(
            pid,
            "features: remove link and undo/redo",
            edited,
            COLORED,
            &[Key::new("press-button", "Remove link")],
        )?;
        self.rich_change(
            pid,
            "features: No Fill preserves font color and undo/redo",
            COLORED,
            FONT_ONLY,
            &[
                Key::new("press-button", "Fill Color menu"),
                Key::new("press-button", "No Fill"),
            ],
        )?;
        self.rich_change(
            pid,
            "features: Automatic font color and undo/redo",
            FONT_ONLY,
            PLAIN,
            &[
                Key::new("press-button", "Font Color menu"),
                Key::new("press-button", "Automatic"),
            ],
        )?;

        self.soft_breaks(pid)?;
        self.quotes(pid)?;
        self.numbering(pid)
    }

    fn plain_paste(&mut self, pid: u32, source: Rich<'_>) -> Result<()> {
        // Native Command-Shift-V must read the plain representation even when
        // the offer has a valid Textloom envelope, fill, font color, and link.
        self.rich(pid, source)?;
        // Prepare the target using UI actions ONLY, preserving that rich offer.
        // The undo assertion below independently verifies the target's state.
        self.keys(
            pid,
            &[
                Key::new("press-button", "Remove link"),
                Key::new("press-button", "Clear formatting"),
                Key::new("type", "plain target"),
                Key::new("chord", "a"),
            ],
        )?;
        self.rich_change(
            pid,
            "features: native Command-Shift-V plain paste from a rich offer",
            Rich {
                text: "plain target",
                body: "<p>plain target</p>",
            },
            PLAIN,
            &[Key::new("shift-chord", "v")],
        )
    }

    fn soft_breaks(&mut self, pid: u32) -> Result<()> {
        let base = Rich {
            text: "first",
            body: "<p>first</p>",
        };
        self.feature_fixture(pid, base)?;
        self.keys(
            pid,
            &[
                Key::new("shift-chord", "7"),
                END,
                Key::new("shift-keycode", "36"),
                Key::new("type", "second"),
            ],
        )?;
        self.rich(
            pid,
            Rich {
                text: "first\nsecond",
                body: "<ol start=\"1\"><li>first<br>second</li></ol>",
            },
        )?;
        self.feature_import(
            "features: TextEdit keeps a soft break within one numbered item",
            json!({"paragraphs":[{"text":"first\u{2028}second", "list_start":1, "number":1}]}),
        )?;
        self.feature_fixture(pid, base)?;
        self.keys(
            pid,
            &[
                Key::new("chord", "2"),
                END,
                Key::new("shift-keycode", "36"),
                Key::new("type", "second"),
            ],
        )?;
        self.rich(
            pid,
            Rich {
                text: "first\nsecond",
                body: "<h2>first<br>second</h2>",
            },
        )?;
        self.note(
            "features: Shift-Enter keeps one heading and plain LF",
            json!({"plain_utf8_exact":true,"html_exact":true}),
        )
    }

    fn quotes(&mut self, pid: u32) -> Result<()> {
        self.feature_fixture(
            pid,
            Rich {
                text: "quote",
                body: "<p>quote</p>",
            },
        )?;
        self.keys(
            pid,
            &[
                Key::new("press-popup", "Body"),
                Key::new("press-button", "Quote"),
                END,
                Key::new("keycode", "36"),
                Key::new("type", "continued"),
            ],
        )?;
        let quote = Rich {
            text: "quote\ncontinued",
            body: "<blockquote><p>quote</p><p>continued</p></blockquote>",
        };
        self.rich(pid, quote)?;
        self.feature_import("features: Enter continues quotes; TextEdit preserves quoted indentation", json!({"paragraphs":[{"text":"quote", "quote":true},{"text":"continued", "quote":true}]}))?;
        self.keys(pid, &[END, Key::new("keycode", "36")])?;
        self.rich_change(
            pid,
            "features: Enter exits empty quote with undo/redo",
            Rich {
                text: "quote\ncontinued\n",
                body: "<blockquote><p>quote</p><p>continued</p><p><br></p></blockquote>",
            },
            Rich {
                text: "quote\ncontinued\n",
                body: "<blockquote><p>quote</p><p>continued</p></blockquote><p><br></p>",
            },
            &[END, Key::new("keycode", "36")],
        )?;
        self.rich_change(
            pid,
            "features: Backspace at quote start with undo/redo",
            Rich {
                text: "quote\ncontinued\n",
                body: "<blockquote><p>quote</p><p>continued</p></blockquote><p><br></p>",
            },
            Rich {
                text: "quote\ncontinued\n",
                body: "<p>quote</p><blockquote><p>continued</p></blockquote><p><br></p>",
            },
            &[START, Key::new("keycode", "51")],
        )
    }

    fn numbering(&mut self, pid: u32) -> Result<()> {
        let text = "one\ntwo\nthree";
        self.feature_fixture(
            pid,
            Rich {
                text,
                body: "<p>one</p><p>two</p><p>three</p>",
            },
        )?;
        self.action(pid, TITLE, "shift-chord", "7")?;
        let continuous = Rich {
            text,
            body: "<ol start=\"1\"><li>one</li><li>two</li><li>three</li></ol>",
        };
        let restarted = Rich {
            text,
            body: "<ol start=\"1\"><li>one</li><li>two</li><li value=\"1\">three</li></ol>",
        };
        self.rich(pid, continuous)?;
        self.rich_change(
            pid,
            "features: Restart at 1 changes focused item with undo/redo",
            continuous,
            restarted,
            &[END, Key::new("press-button", "Restart at 1")],
        )?;
        self.rich_change(
            pid,
            "features: Continue numbering with undo/redo",
            restarted,
            continuous,
            &[END, Key::new("press-button", "Continue numbering")],
        )?;
        self.keys(
            pid,
            &[
                START,
                Key::new("keycode", "125"),
                Key::new("command-shift-keycode", "125"),
            ],
        )?;
        self.copy(
            pid,
            TITLE,
            "two\nthree",
            ClipboardCheck::Plain,
            CopyScope::Selection,
        )?;
        let offer = self.clipboard("native-html", &[], None)?;
        check_html(
            offer.get("html").ok_or("Native list offer has no HTML")?,
            "<ol start=\"2\"><li>two</li><li>three</li></ol>",
        )?;
        self.feature_import("features: TextEdit keeps copied item numbers 2 and 3", json!({"paragraphs":[{"text":"two","list_start":2,"number":2},{"text":"three","list_start":2,"number":3}]}))?;
        self.rich_change(pid, "features: numbering continues across body text", continuous,
            Rich { text, body: "<ol start=\"1\"><li>one</li></ol><p>two</p><ol start=\"2\"><li>three</li></ol>" },
            &[START, Key::new("keycode", "125"), Key::new("chord", "0")])?;
        self.keys(
            pid,
            &[START, Key::new("keycode", "125"), Key::new("chord", "1")],
        )?;
        self.rich(pid, Rich { text, body: "<ol start=\"1\"><li>one</li></ol><h1>two</h1><ol start=\"1\"><li>three</li></ol>" })?;
        self.note(
            "features: heading restarts subsequent numbering",
            json!({"html_exact":true}),
        )
    }

    pub(super) fn forced_wrap(&mut self, pid: u32) -> Result<()> {
        let fixture = (0..200)
            .map(|index| format!("word{index:03} "))
            .collect::<String>();
        self.shortcut_fixture(pid, &fixture)?;
        self.keys(pid, &[END, Key::new("command-shift-keycode", "123")])?;
        let generation = change_count(&self.clipboard("state", &[], None)?)?;
        self.action(pid, TITLE, "chord", "c")?;
        self.clipboard("state", &[], Some(generation))?;
        let copied = self.run(&strings(&PASTE), None, 10.0, Exit::MustSucceed)?;
        let suffix = String::from_utf8(copied.stdout).map_err(|error| error.to_string())?;
        let prefix = wrapped_prefix(&fixture, &suffix)?;
        self.shortcut_check(
            pid,
            "forced-wrap Command-Backspace deletes only the observed final visual line",
            prefix,
            &[END, Key::new("command-keycode", "51")],
            Some(&fixture),
        )?;
        self.note(
            "features: forced-wrap boundary witnessed independently of font metrics",
            json!({"fixture_bytes":fixture.len(), "last_visual_line_bytes":suffix.len()}),
        )
    }
}

#[cfg(test)]
mod tests;
