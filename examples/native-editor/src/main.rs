//! A native desktop host for Textloom's egui widget, used for native QA.
//!
//! This eframe application opens a window with a rich text document, a
//! formatting toolbar, a link field, and Find and Replace fields, on Linux,
//! macOS, and Windows. Its flags (see [`qa::HELP`]) add a smoke test that
//! closes the window after 20 frames, a content-free QA report, and a native
//! rich clipboard. The same executable also runs the rich clipboard's helper
//! processes (see [`clipboard::helper`]). CI workflows and `scripts/qa-tools`
//! parse the host's output and reports, so their wording and fields must stay
//! stable.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{io, process};

use eframe::egui;

use crate::clipboard::NativeClipboard;
use crate::native_editor::NativeEditor;
use crate::qa::ReportFile;

mod clipboard;
mod ime_handoff;
mod native_editor;
mod qa;

/// Runs a clipboard helper if the arguments request one, and otherwise the
/// host window.
///
/// Exits with status 1 when a clipboard helper fails, and with status 2 for
/// invalid arguments or an unusable report path. Returns an error when a font
/// cannot be read, the window cannot open, or the smoke test, clipboard
/// self-test, or report write failed.
fn main() -> eframe::Result {
    let raw_arguments: Vec<OsString> = qa::arguments().collect();
    if let Some(result) = clipboard::helper(&raw_arguments) {
        if let Err(error) = result {
            eprintln!("{error}");
            process::exit(1);
        }
        return Ok(());
    }
    let arguments = match qa::Options::parse(raw_arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("{error}");
            process::exit(2);
        }
    };
    if arguments.help {
        println!("{}", qa::HELP);
        return Ok(());
    }
    let regular_font = qa::read_font(arguments.regular_font.as_deref())?;
    let bold_font = qa::read_font(arguments.bold_font.as_deref())?;
    let report = match &arguments.qa_report {
        Some(path) => match ReportFile::create(path) {
            Ok(report) => Some(report),
            Err(error) => {
                eprintln!("Cannot create QA report {}: {error}", path.display());
                process::exit(2);
            }
        },
        None => None,
    };
    let failed = Arc::new(AtomicBool::new(false));
    let app_failed = Arc::clone(&failed);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TextLoom native example")
            .with_app_id("org.textloom.NativeExample")
            .with_inner_size([920.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "TextLoom native example",
        options,
        Box::new(move |context| {
            native_editor::configure_context(&context.egui_ctx);
            if let Some(theme) = arguments.theme {
                context.egui_ctx.set_theme(theme);
            }
            let mut fonts = egui::FontDefinitions::default();
            if let Some(bytes) = regular_font {
                fonts.font_data.insert(
                    "custom-regular".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "custom-regular".into());
            }
            if let Some(bytes) = bold_font {
                fonts.font_data.insert(
                    "custom-bold".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                let mut fallback = fonts.families[&egui::FontFamily::Proportional].clone();
                fallback.insert(0, "custom-bold".into());
                fonts
                    .families
                    .insert(egui::FontFamily::Name("Bold".into()), fallback);
            }
            context.egui_ctx.set_fonts(fonts);
            let clipboard = if arguments.rich_clipboard {
                Some(NativeClipboard::try_new(context).map_err(io::Error::other)?)
            } else {
                None
            };
            Ok(Box::new(NativeEditor::new(
                &arguments, report, app_failed, clipboard,
            )))
        }),
    )?;
    if failed.load(Ordering::Relaxed) {
        return Err(eframe::Error::AppCreation(
            io::Error::other("Native smoke test or QA report failed; see stderr").into(),
        ));
    }
    Ok(())
}
