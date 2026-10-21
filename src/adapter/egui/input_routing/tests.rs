use super::*;
use crate::{Editor, adapter::egui::RichTextEditor};
use egui::{Id, ImeEvent, Pos2, Rect, Ui};
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug)]
enum FieldKind {
    Native,
    Rich,
}

struct Field {
    native: String,
    rich: Editor,
    kind: FieldKind,
}

impl Field {
    fn new(kind: FieldKind, text: &str) -> Self {
        Self {
            native: text.into(),
            rich: Editor::from_text(text),
            kind,
        }
    }

    fn text(&self) -> String {
        match self.kind {
            FieldKind::Native => self.native.clone(),
            FieldKind::Rich => self.rich.document().plain_text(),
        }
    }

    fn show(&mut self, ui: &mut Ui, name: &str) {
        match self.kind {
            FieldKind::Native => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.native)
                        .id(Id::new(name))
                        .cursor_at_end(false),
                );
            }
            FieldKind::Rich => {
                let output = RichTextEditor::new(&mut self.rich)
                    .id(Id::new(name))
                    .show(ui);
                assert!(output.errors.is_empty());
                assert!(output.accessibility_errors.is_empty());
            }
        }
    }
}

struct Fields {
    fields: [Field; 2],
    names: [&'static str; 2],
    reversed: bool,
    second_enabled: bool,
}

impl Fields {
    fn new(first: FieldKind, second: FieldKind) -> Self {
        Self {
            fields: [Field::new(first, "a"), Field::new(second, "b")],
            names: ["first", "second"],
            reversed: false,
            second_enabled: true,
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            for index in if self.reversed { [1, 0] } else { [0, 1] } {
                ui.add_enabled_ui(index == 0 || self.second_enabled, |ui| {
                    self.fields[index].show(ui, self.names[index]);
                });
            }
        });
    }

    fn frame(&mut self, ctx: &Context, events: Vec<Event>) -> FullOutput {
        ctx.run_ui(raw(events), |ui| self.show(ui))
    }

    fn warm(&mut self, ctx: &Context) {
        ctx.memory_mut(|memory| memory.request_focus(Id::new(self.names[0])));
        self.frame(ctx, vec![]).drop_without_applying_deltas();
        self.frame(ctx, vec![]).drop_without_applying_deltas();
    }

    fn flush(&mut self, ctx: &Context) {
        for _ in 0..8 {
            if ctx.plugin::<InputRouting>().lock().pending.is_empty() {
                return;
            }
            self.frame(ctx, vec![]).drop_without_applying_deltas();
        }
        panic!("input routing did not drain its pending events");
    }
}

fn raw(events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 400.0))),
        events,
        ..Default::default()
    }
}

fn focus(name: &str) -> Event {
    Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Focus,
        target_node: Id::new(name).accesskit_id(),
        target_tree: egui::accesskit::TreeId::ROOT,
        data: None,
    })
}

fn text(value: &str) -> Event {
    Event::Text(value.into())
}

fn preedit(value: &str) -> Event {
    Event::Ime(ImeEvent::Preedit {
        text: value.into(),
        active_range_chars: None,
    })
}

fn commit(value: &str) -> Event {
    Event::Ime(ImeEvent::Commit(value.into()))
}

fn context(max_passes: usize) -> Context {
    let ctx = Context::default();
    ctx.enable_accesskit();
    install_input_routing(&ctx);
    ctx.options_mut(|options| options.max_passes = NonZeroUsize::new(max_passes).unwrap());
    ctx
}

#[test]
fn focus_actions_route_each_input_segment_to_its_field() {
    for first in [FieldKind::Native, FieldKind::Rich] {
        for second in [FieldKind::Native, FieldKind::Rich] {
            for reversed in [false, true] {
                for max_passes in [1, 2, 8] {
                    for (events, expected, focused) in [
                        (vec![focus("second"), text("x")], ["a", "xb"], "second"),
                        (vec![text("x"), focus("second")], ["xa", "b"], "second"),
                        (
                            vec![
                                text("x"),
                                focus("second"),
                                text("y"),
                                focus("first"),
                                text("z"),
                            ],
                            ["xza", "yb"],
                            "first",
                        ),
                        (
                            vec![focus("second"), focus("first"), text("x")],
                            ["xa", "b"],
                            "first",
                        ),
                    ] {
                        let ctx = context(max_passes);
                        let mut fields = Fields::new(first, second);
                        fields.reversed = reversed;
                        fields.warm(&ctx);
                        let output = fields.frame(&ctx, events);
                        if max_passes >= 2 && expected == ["a", "xb"] {
                            assert_eq!(output.platform_output.num_completed_passes, 2);
                            assert_eq!(fields.fields[1].text(), "xb");
                        }
                        output.drop_without_applying_deltas();
                        fields.flush(&ctx);
                        assert_eq!(fields.fields.each_ref().map(Field::text), expected);
                        assert_eq!(
                            ctx.memory(|memory| memory.focused()),
                            Some(Id::new(focused))
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn ime_input_after_a_focus_request_reaches_the_newly_focused_field() {
    for second in [FieldKind::Native, FieldKind::Rich] {
        for reversed in [false, true] {
            for max_passes in [1, 2, 8] {
                for ime in [
                    vec![commit("x")],
                    vec![preedit("x")],
                    vec![preedit("x"), commit("x")],
                ] {
                    let committed = matches!(ime.last(), Some(Event::Ime(ImeEvent::Commit(_))));
                    let ctx = context(max_passes);
                    let mut fields = Fields::new(FieldKind::Rich, second);
                    fields.reversed = reversed;
                    fields.warm(&ctx);
                    let mut events = vec![focus("second")];
                    events.extend(ime);
                    fields.frame(&ctx, events).drop_without_applying_deltas();
                    fields.flush(&ctx);
                    assert_eq!(fields.fields[0].text(), "a");
                    assert!(fields.fields[0].rich.composition().is_none());
                    if committed {
                        assert_eq!(fields.fields[1].text(), "xb");
                    }
                    assert_eq!(
                        ctx.memory(|memory| memory.focused()),
                        Some(Id::new("second"))
                    );
                }
            }
        }
    }
}

#[test]
fn ime_input_after_tab_reaches_the_next_field() {
    let ctx = context(1);
    let mut fields = Fields::new(FieldKind::Rich, FieldKind::Native);
    fields.fields = [
        Field::new(FieldKind::Rich, "abc"),
        Field::new(FieldKind::Native, ""),
    ];
    fields.warm(&ctx);
    let tab = Event::Key {
        key: egui::Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    fields
        .frame(&ctx, vec![commit("x"), tab, preedit("n")])
        .drop_without_applying_deltas();
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(Id::new("second"))
    );
    assert_eq!(fields.fields[0].text(), "xabc");
    assert!(fields.fields[0].rich.composition().is_none());
    fields
        .frame(&ctx, vec![preedit(""), commit("n")])
        .drop_without_applying_deltas();
    fields.flush(&ctx);
    assert_eq!(fields.fields.each_ref().map(Field::text), ["xabc", "n"]);
}

#[test]
fn rejected_focus_requests_keep_the_original_composition_and_input() {
    for rejected in 0..4 {
        let ctx = context(1);
        let mut fields = Fields::new(FieldKind::Rich, FieldKind::Native);
        fields.second_enabled = rejected != 3;
        fields.warm(&ctx);
        fields.fields[0]
            .rich
            .update_composition("한", None)
            .unwrap();
        let mut event = focus(if rejected == 0 { "missing" } else { "second" });
        let Event::AccessKitActionRequest(request) = &mut event else {
            unreachable!()
        };
        match rejected {
            1 => {
                request.target_tree =
                    egui::accesskit::TreeId("00000000-0000-0000-0000-000000000001".parse().unwrap())
            }
            2 => request.data = Some(egui::accesskit::ActionData::Value("invalid".into())),
            _ => {}
        }
        fields
            .frame(&ctx, vec![event])
            .drop_without_applying_deltas();
        assert!(fields.fields[0].rich.composition().is_some());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("first"))
        );
        fields
            .frame(&ctx, vec![Event::Ime(ImeEvent::Commit("한".into()))])
            .drop_without_applying_deltas();
        fields.flush(&ctx);
        assert_eq!(fields.fields[0].text(), "한a");
        assert_eq!(fields.fields[1].text(), "b");
    }
}

#[test]
fn queued_input_precedes_new_input_and_requests_repaint_at_the_pass_limit() {
    let ctx = context(1);
    let mut fields = Fields::new(FieldKind::Native, FieldKind::Native);
    fields.warm(&ctx);
    let output = fields.frame(&ctx, vec![focus("second"), text("x")]);
    assert_eq!(fields.fields[1].text(), "b");
    assert!(output.platform_output.requested_discard());
    assert_eq!(
        output.viewport_output[&ViewportId::ROOT].repaint_delay,
        Duration::ZERO
    );
    output.drop_without_applying_deltas();
    fields
        .frame(&ctx, vec![text("y")])
        .drop_without_applying_deltas();
    assert_eq!(fields.fields[1].text(), "xb");
    fields.flush(&ctx);
    assert_eq!(fields.fields[0].text(), "a");
    assert_eq!(fields.fields[1].text(), "xyb");
    assert!(ctx.plugin::<InputRouting>().lock().pending.is_empty());
}

#[test]
fn queued_input_keeps_its_window_focus_and_precedes_later_pointer_input() {
    for target in [FieldKind::Native, FieldKind::Rich] {
        for click in [false, true] {
            let ctx = context(1);
            let mut fields = Fields::new(FieldKind::Rich, target);
            fields.warm(&ctx);
            fields
                .frame(&ctx, vec![focus("second"), text("x")])
                .drop_without_applying_deltas();
            let mut input = raw(vec![]);
            if click {
                let position = ctx.read_response(Id::new("first")).unwrap().rect.center();
                input.events = vec![
                    Event::PointerMoved(position),
                    Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
            } else {
                input.focused = false;
            }
            ctx.run_ui(input.clone(), |ui| fields.show(ui))
                .drop_without_applying_deltas();
            assert_eq!(fields.fields[0].text(), "a");
            assert_eq!(fields.fields[1].text(), "xb");
            assert!(ctx.input(|input| input.focused));
            input.events.clear();
            ctx.run_ui(input, |ui| fields.show(ui))
                .drop_without_applying_deltas();
            assert_eq!(fields.fields[0].text(), "a");
            assert_eq!(fields.fields[1].text(), "xb");
            assert_eq!(ctx.input(|input| input.focused), click);
            if click {
                assert_eq!(
                    ctx.memory(|memory| memory.focused()),
                    Some(Id::new("first"))
                );
            }
            assert!(ctx.plugin::<InputRouting>().lock().pending.is_empty());
        }
    }
}

#[test]
fn automatic_installation_and_manual_passes_preserve_focus_order() {
    let ctx = Context::default();
    ctx.enable_accesskit();
    let mut fields = Fields::new(FieldKind::Rich, FieldKind::Native);
    fields.warm(&ctx);
    assert!(ctx.plugin_opt::<InputRouting>().is_some());
    assert!(ctx.plugin::<InputRouting>().lock().passes.is_empty());
    for events in [vec![focus("second"), text("x")], vec![]] {
        ctx.begin_pass(raw(events));
        let mut ui = Ui::new(ctx.clone(), Id::new("manual root"), egui::UiBuilder::new());
        fields.show(&mut ui);
        ctx.end_pass().drop_without_applying_deltas();
    }
    assert_eq!(fields.fields[0].text(), "a");
    assert_eq!(fields.fields[1].text(), "xb");
    assert!(ctx.plugin::<InputRouting>().lock().passes.is_empty());
}

#[test]
fn nested_viewports_keep_their_own_queued_events_and_repaint_requests() {
    let ctx = context(1);
    ctx.set_embed_viewports(false);
    let mut parent = Fields::new(FieldKind::Native, FieldKind::Native);
    let mut child = Fields::new(FieldKind::Native, FieldKind::Native);
    child.names = ["child first", "child second"];
    let child_id = ViewportId::from_hash_of("child viewport");
    for pass in 0..4 {
        let mut parent_input = raw(if pass == 2 {
            vec![focus("second"), text("p")]
        } else {
            vec![]
        });
        parent_input.viewports.insert(
            child_id,
            egui::ViewportInfo {
                parent: Some(ViewportId::ROOT),
                ..Default::default()
            },
        );
        ctx.begin_pass(parent_input.clone());
        if pass == 0 {
            ctx.memory_mut(|memory| memory.request_focus(Id::new("first")));
        }
        let mut parent_ui = Ui::new(ctx.clone(), Id::new("parent ui"), egui::UiBuilder::new());
        parent.show(&mut parent_ui);
        ctx.show_viewport_deferred(child_id, egui::ViewportBuilder::default(), |_, _| {});

        let mut child_input = parent_input;
        child_input.viewport_id = child_id;
        child_input.events = if pass == 2 {
            vec![focus("child second"), text("c")]
        } else {
            vec![]
        };
        ctx.begin_pass(child_input);
        if pass == 0 {
            ctx.memory_mut(|memory| memory.request_focus(Id::new("child first")));
        }
        let mut child_ui = Ui::new(ctx.clone(), Id::new("child ui"), egui::UiBuilder::new());
        child.show(&mut child_ui);
        let output = ctx.end_pass();
        if pass == 2 {
            assert_eq!(
                output.viewport_output[&child_id].repaint_delay,
                Duration::ZERO
            );
        }
        output.drop_without_applying_deltas();
        let output = ctx.end_pass();
        if pass == 2 {
            assert_eq!(
                output.viewport_output[&ViewportId::ROOT].repaint_delay,
                Duration::ZERO
            );
        }
        output.drop_without_applying_deltas();
    }
    assert_eq!(parent.fields.each_ref().map(Field::text), ["a", "pb"]);
    assert_eq!(child.fields.each_ref().map(Field::text), ["a", "cb"]);
    let routing = ctx.plugin::<InputRouting>();
    let routing = routing.lock();
    assert!(routing.pending.is_empty());
    assert!(routing.passes.is_empty());
}

#[test]
fn removed_viewports_discard_their_undeliverable_input() {
    let ctx = context(1);
    let child_id = ViewportId::from_hash_of("removed child");
    let mut routing = InputRouting::default();
    let mut parent = raw(vec![]);
    routing.input_hook(&ctx, &mut parent);
    let mut child = raw(vec![focus("child"), text("pending")]);
    child.viewport_id = child_id;
    routing.input_hook(&ctx, &mut child);
    // An inner viewport's output must not discard its parent's or siblings' input.
    routing.output_hook(&ctx, &mut FullOutput::default());
    assert!(routing.pending.contains_key(&child_id));
    // The outermost output is authoritative about which viewports are still alive.
    routing.output_hook(&ctx, &mut FullOutput::default());
    assert!(routing.pending.is_empty());
}
