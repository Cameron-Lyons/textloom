//! Preserves native Windows Ctrl+Alt text before egui-winit drops it.
//!
//! Only a real printable character reported with all native modifiers is
//! normalized. The original modifiers are restored immediately, in event order.
//! Other platforms keep eframe's ordinary event loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use winit::event::{ElementState, Ime, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement as _;
use winit::window::WindowId;

#[derive(Debug, Default)]
struct WindowInput {
    modifiers: Modifiers,
    composing: bool,
}

impl WindowInput {
    fn text<'text>(
        &self,
        key: &Key,
        state: ElementState,
        synthetic: bool,
        produced: Option<&'text str>,
    ) -> Option<&'text str> {
        let modifiers = self.modifiers.state();
        if synthetic
            || state != ElementState::Pressed
            || self.composing
            || !matches!(
                key,
                Key::Character(_) | Key::Dead(_) | Key::Named(NamedKey::Space)
            )
            || !modifiers.contains(ModifiersState::CONTROL | ModifiersState::ALT)
            || modifiers.super_key()
        {
            return None;
        }
        produced.filter(|text| {
            !text.is_empty()
                && text.chars().all(|character| {
                    !character.is_control()
                        && !matches!(
                            character,
                            '\u{e000}'..='\u{f8ff}'
                                | '\u{f0000}'..='\u{ffffd}'
                                | '\u{100000}'..='\u{10fffd}'
                        )
                })
        })
    }
}

#[derive(Debug, Default)]
struct InputRouter {
    windows: HashMap<WindowId, WindowInput>,
}

impl InputRouter {
    fn forward(
        &mut self,
        window: WindowId,
        mut event: WindowEvent,
        mut deliver: impl FnMut(WindowEvent),
    ) {
        match &mut event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.windows.entry(window).or_default().modifiers = *modifiers;
            }
            WindowEvent::Focused(false) | WindowEvent::Destroyed => {
                self.windows.remove(&window);
            }
            WindowEvent::Ime(ime) => {
                let state = self.windows.entry(window).or_default();
                match ime {
                    Ime::Preedit(text, _) => state.composing = !text.is_empty(),
                    Ime::Commit(_) | Ime::Disabled => state.composing = false,
                    Ime::Enabled => {}
                }
            }
            WindowEvent::KeyboardInput {
                event: key,
                is_synthetic,
                ..
            } => {
                if let Some(state) = self.windows.get(&window)
                    && let Some(text) = state.text(
                        &key.logical_key,
                        key.state,
                        *is_synthetic,
                        key.text_with_all_modifiers(),
                    )
                {
                    // Do not infer text from a layout-derived logical key or a
                    // Ctrl-filtered text field. Keep the actual Windows text,
                    // including multi-character dead-key pairs.
                    key.text = Some(text.into());
                    with_text_modifiers(state.modifiers, event, deliver);
                    return;
                }
            }
            _ => {}
        }
        deliver(event);
    }
}

fn with_text_modifiers(
    original: Modifiers,
    event: WindowEvent,
    mut deliver: impl FnMut(WindowEvent),
) {
    let text_modifiers = original
        .state()
        .difference(ModifiersState::CONTROL | ModifiersState::ALT);
    deliver(WindowEvent::ModifiersChanged(text_modifiers.into()));
    deliver(event);
    deliver(WindowEvent::ModifiersChanged(original));
}

#[derive(Debug, Default)]
struct ExitStatus {
    root: Option<WindowId>,
    created: bool,
    creation_error: Option<String>,
    root_close_requested: bool,
}

impl ExitStatus {
    fn window_event(&mut self, window: WindowId, event: &WindowEvent) {
        if self.root == Some(window) && matches!(event, WindowEvent::CloseRequested) {
            self.root_close_requested = true;
        }
    }

    fn finish(&self, teardown_before_exit: bool) -> eframe::Result {
        let message = if let Some(error) = &self.creation_error {
            Some(error.clone())
        } else if !self.created {
            Some("Native graphics initialization ended before the application was created".into())
        } else if !teardown_before_exit {
            Some(if self.root_close_requested {
                "Native event loop exited before completing the application close request".into()
            } else {
                "Native event loop exited without completing an application close".into()
            })
        } else {
            None
        };
        match message {
            Some(message) => Err(eframe::Error::AppCreation(
                std::io::Error::other(message).into(),
            )),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Default)]
struct ExitLifecycle {
    exiting: AtomicBool,
    teardown_before_exit: AtomicBool,
}

struct ExitObserver(Arc<ExitLifecycle>);

impl egui::Plugin for ExitObserver {
    fn debug_name(&self) -> &'static str {
        "textloom_native_exit"
    }

    fn on_exit(&mut self, _context: &egui::Context) {
        // In eframe 0.37 an accepted root close tears down the egui context
        // during event handling, including when a minimized window runs only
        // App::logic. On an internal error, eframe first exits the event loop
        // and tears down the context from ApplicationHandler::exiting instead.
        // A queued Close command alone does not establish successful shutdown.
        if !self.0.exiting.load(Ordering::Relaxed) {
            self.0.teardown_before_exit.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs the Windows host with a narrowly scoped native text normalization.
#[cfg(target_os = "windows")]
pub(crate) fn run_native(
    name: &str,
    mut options: eframe::NativeOptions,
    creator: eframe::AppCreator<'_>,
) -> eframe::Result {
    use std::cell::RefCell;
    use std::rc::Rc;

    use winit::event_loop::EventLoop;
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;

    let mut builder = EventLoop::<eframe::UserEvent>::with_user_event();
    if let Some(hook) = options.event_loop_builder.take() {
        hook(&mut builder);
    }
    let mut event_loop = builder.build()?;
    let run_and_return = options.run_and_return;
    let status = Rc::new(RefCell::new(ExitStatus::default()));
    let app_status = Rc::clone(&status);
    let lifecycle = Arc::new(ExitLifecycle::default());
    let app_lifecycle = Arc::clone(&lifecycle);
    let app = eframe::create_native(
        name,
        options,
        Box::new(move |context| {
            let result = creator(context);
            let mut status = app_status.borrow_mut();
            match &result {
                Ok(_) => {
                    status.created = true;
                    status.root = context.winit_window().map(|window| window.id());
                    context.egui_ctx.add_plugin(ExitObserver(app_lifecycle));
                }
                Err(error) => status.creation_error = Some(error.to_string()),
            }
            result
        }),
        &event_loop,
    );
    let mut host = WindowsHost {
        app,
        input: InputRouter::default(),
        status: Rc::clone(&status),
        lifecycle: Arc::clone(&lifecycle),
    };
    // In eframe 0.37 create_native always uses its returning wrapper, even
    // when run_and_return is false. Both winit entry points return on Windows;
    // keep both paths through validation below and the caller's QA error check.
    if run_and_return {
        event_loop.run_app_on_demand(&mut host)?;
    } else {
        event_loop.run_app(&mut host)?;
    }
    // create_native does not expose eframe's internal return_result. Preserve
    // nonzero exit status for startup failures and unexplained runtime exits.
    status
        .borrow()
        .finish(lifecycle.teardown_before_exit.load(Ordering::Relaxed))
}

#[cfg(target_os = "windows")]
struct WindowsHost<'app> {
    app: eframe::EframeWinitApplication<'app>,
    input: InputRouter,
    status: std::rc::Rc<std::cell::RefCell<ExitStatus>>,
    lifecycle: Arc<ExitLifecycle>,
}

#[cfg(target_os = "windows")]
impl winit::application::ApplicationHandler<eframe::UserEvent> for WindowsHost<'_> {
    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window: WindowId,
        event: WindowEvent,
    ) {
        self.status.borrow_mut().window_event(window, &event);
        self.input.forward(window, event, |event| {
            self.app.window_event(event_loop, window, event);
        });
    }

    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.app.resumed(event_loop);
    }

    fn suspended(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.app.suspended(event_loop);
    }

    fn new_events(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        self.app.new_events(event_loop, cause);
    }

    fn user_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        event: eframe::UserEvent,
    ) {
        self.app.user_event(event_loop, event);
    }

    fn device_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        device: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.app.device_event(event_loop, device, event);
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
    }

    fn exiting(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.lifecycle.exiting.store(true, Ordering::Relaxed);
        self.app.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        self.app.memory_warning(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_alt() -> WindowInput {
        WindowInput {
            modifiers: (ModifiersState::CONTROL | ModifiersState::ALT).into(),
            composing: false,
        }
    }

    #[test]
    fn normalized_text_reaches_document_and_field_once_with_modifiers_restored() {
        let held = egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        for field_focused in [false, true] {
            let context = egui::Context::default();
            context.set_os(egui::os::OperatingSystem::Windows);
            crate::native_editor::configure_context(&context);
            let mut editor = textloom::Editor::default();
            let mut field = String::new();
            let document_id = egui::Id::unique("ctrl-alt-document");
            let field_id = egui::Id::unique("ctrl-alt-field");
            {
                let mut frame = |events| {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(600.0, 400.0),
                        )),
                        events,
                        focused: true,
                        ..Default::default()
                    };
                    let output = context.run_ui(input, |ui| {
                        ui.memory_mut(|memory| {
                            memory.request_focus(if field_focused { field_id } else { document_id })
                        });
                        let response = textloom::adapter::egui::RichTextEditor::new(&mut editor)
                            .id(document_id)
                            .show(ui);
                        assert!(response.errors.is_empty());
                        ui.add(egui::TextEdit::singleline(&mut field).id(field_id));
                    });
                    assert!(
                        !output.viewport_output.values().any(|viewport| viewport
                            .commands
                            .contains(&egui::ViewportCommand::Close))
                    );
                    output.drop_without_applying_deltas();
                };
                frame(Vec::new());
                // This is egui-winit's output for the scoped native sequence.
                // The native KeyEvent constructor is private to winit, so its
                // OS translation remains covered by the native Windows probe.
                frame(vec![
                    egui::Event::ModifiersChanged(held),
                    egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                    egui::Event::Key {
                        key: egui::Key::Q,
                        physical_key: Some(egui::Key::Q),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::Text("@".into()),
                    egui::Event::ModifiersChanged(held),
                    egui::Event::Key {
                        key: egui::Key::Q,
                        physical_key: Some(egui::Key::Q),
                        pressed: false,
                        repeat: false,
                        modifiers: held,
                    },
                ]);
            }
            assert_eq!(
                editor.document().plain_text(),
                if field_focused { "" } else { "@" }
            );
            assert_eq!(field, if field_focused { "@" } else { "" });
            assert_eq!(context.input(|input| input.modifiers), held);
        }
    }

    #[test]
    fn only_actual_printable_ctrl_alt_text_is_normalized() {
        let state = ctrl_alt();
        let key = Key::Character("q".into());
        for text in ["@", "€", "´x", "a\u{301}", "世界"] {
            assert_eq!(
                state.text(&key, ElementState::Pressed, false, Some(text)),
                Some(text)
            );
        }
        assert_eq!(
            state.text(
                &Key::Named(NamedKey::Space),
                ElementState::Pressed,
                false,
                Some("\u{a0}")
            ),
            Some("\u{a0}")
        );
        for text in [
            None,
            Some(""),
            Some("\r"),
            Some("\u{1}"),
            Some("x\u{7f}"),
            Some("\u{e000}"),
            Some("\u{f0000}"),
            Some("\u{100000}"),
        ] {
            assert_eq!(state.text(&key, ElementState::Pressed, false, text), None);
        }
        assert_eq!(
            state.text(&key, ElementState::Released, false, Some("@")),
            None
        );
        assert_eq!(
            state.text(&key, ElementState::Pressed, true, Some("@")),
            None
        );
        let dead = Key::Dead(Some('´'));
        assert_eq!(state.text(&dead, ElementState::Pressed, false, None), None);
        assert_eq!(
            state.text(&dead, ElementState::Pressed, false, Some("´")),
            Some("´")
        );
        for key in [
            Key::Named(winit::keyboard::NamedKey::Process),
            Key::Named(winit::keyboard::NamedKey::Enter),
        ] {
            assert_eq!(
                state.text(&key, ElementState::Pressed, false, Some("@")),
                None
            );
        }
        let composing = WindowInput {
            composing: true,
            ..ctrl_alt()
        };
        assert_eq!(
            composing.text(&key, ElementState::Pressed, false, Some("@")),
            None
        );
    }

    #[test]
    fn ordinary_shortcuts_and_physical_altgr_are_unchanged() {
        let key = Key::Character("q".into());
        for modifiers in [
            ModifiersState::empty(),
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
        ] {
            let state = WindowInput {
                modifiers: modifiers.into(),
                composing: false,
            };
            assert_eq!(
                state.text(&key, ElementState::Pressed, false, Some("@")),
                None
            );
        }
    }

    #[test]
    fn text_modifiers_are_scoped_in_order_and_preserve_shift() {
        let original: Modifiers =
            (ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT).into();
        let mut events = Vec::new();
        with_text_modifiers(
            original,
            WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
            |event| events.push(event),
        );
        assert!(
            matches!(events[0], WindowEvent::ModifiersChanged(modifiers) if modifiers.state() == ModifiersState::SHIFT)
        );
        assert!(matches!(events[1], WindowEvent::CursorLeft { .. }));
        assert!(
            matches!(events[2], WindowEvent::ModifiersChanged(modifiers) if modifiers == original)
        );
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn window_modifiers_and_composition_reset_on_focus_loss() {
        let first = WindowId::from(1);
        let second = WindowId::from(2);
        let mut router = InputRouter::default();
        router.forward(
            first,
            WindowEvent::ModifiersChanged(ctrl_alt().modifiers),
            |_| {},
        );
        router.forward(
            second,
            WindowEvent::ModifiersChanged(ModifiersState::SHIFT.into()),
            |_| {},
        );
        router.forward(
            first,
            WindowEvent::Ime(Ime::Preedit("かな".into(), None)),
            |_| {},
        );
        assert!(router.windows[&first].composing);
        assert!(!router.windows[&second].composing);
        assert_eq!(
            router.windows[&second].modifiers.state(),
            ModifiersState::SHIFT
        );
        router.forward(first, WindowEvent::Ime(Ime::Commit("仮名".into())), |_| {});
        assert!(!router.windows[&first].composing);
        router.forward(first, WindowEvent::Focused(false), |_| {});
        assert!(!router.windows.contains_key(&first));
        assert!(router.windows.contains_key(&second));
        router.forward(second, WindowEvent::Destroyed, |_| {});
        assert!(router.windows.is_empty());
    }

    #[test]
    fn native_close_must_belong_to_root_and_be_accepted() {
        let root = WindowId::from(1);
        let mut status = ExitStatus {
            root: Some(root),
            created: true,
            ..Default::default()
        };
        status.window_event(WindowId::from(2), &WindowEvent::CloseRequested);
        assert!(!status.root_close_requested);
        assert!(status.finish(false).is_err());
        status.window_event(root, &WindowEvent::CloseRequested);
        assert!(status.root_close_requested);
        assert!(status.finish(false).is_err());
        assert!(status.finish(true).is_ok());
        assert!(ExitStatus::default().finish(true).is_err());
        status.creation_error = Some("clipboard initialization failed".into());
        assert!(
            status
                .finish(true)
                .unwrap_err()
                .to_string()
                .contains("clipboard initialization failed")
        );
    }

    #[test]
    fn close_requests_do_not_count_as_completed_teardown() {
        let context = egui::Context::default();
        let lifecycle = Arc::new(ExitLifecycle::default());
        context.add_plugin(ExitObserver(Arc::clone(&lifecycle)));
        context
            .run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));
        context
            .run_ui(egui::RawInput::default(), |ui| {
                ui.ctx().send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("secondary"),
                    egui::ViewportCommand::Close,
                );
            })
            .textures_delta
            .clear();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));
        context
            .run_ui(egui::RawInput::default(), |ui| {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            })
            .textures_delta
            .clear();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));

        // A renderer failure after the Close command, before its acceptance,
        // cleans up only from the event loop's exiting callback.
        lifecycle.exiting.store(true, Ordering::Relaxed);
        context.on_exit();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));
        assert!(
            ExitStatus {
                created: true,
                ..Default::default()
            }
            .finish(lifecycle.teardown_before_exit.load(Ordering::Relaxed))
            .is_err()
        );
    }

    #[test]
    fn canceled_close_does_not_count_as_completed_teardown() {
        let context = egui::Context::default();
        let lifecycle = Arc::new(ExitLifecycle::default());
        context.add_plugin(ExitObserver(Arc::clone(&lifecycle)));
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut output = context.run_ui(input, |ui| {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        });
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        output.textures_delta.clear();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));

        lifecycle.exiting.store(true, Ordering::Relaxed);
        context.on_exit();
        assert!(!lifecycle.teardown_before_exit.load(Ordering::Relaxed));
    }

    #[test]
    fn accepted_close_teardown_is_observed_without_an_egui_pass() {
        let context = egui::Context::default();
        let lifecycle = Arc::new(ExitLifecycle::default());
        context.add_plugin(ExitObserver(Arc::clone(&lifecycle)));
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = context.run_logic(&input, |context| {
            assert!(context.input(|input| input.viewport().close_requested()));
        });
        assert!(
            !output
                .viewport_commands
                .get(&egui::ViewportId::ROOT)
                .is_some_and(|commands| commands.contains(&egui::ViewportCommand::CancelClose))
        );
        assert_eq!(context.cumulative_pass_nr(), 0);

        // eframe accepts this logic-only close and tears down the context
        // before the event loop reports exiting.
        context.on_exit();
        lifecycle.exiting.store(true, Ordering::Relaxed);
        assert!(lifecycle.teardown_before_exit.load(Ordering::Relaxed));
        assert!(
            ExitStatus {
                created: true,
                ..Default::default()
            }
            .finish(lifecycle.teardown_before_exit.load(Ordering::Relaxed))
            .is_ok()
        );
    }
}
