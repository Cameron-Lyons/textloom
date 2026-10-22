use std::collections::{HashMap, VecDeque};
use std::mem;
use std::time::Duration;

use egui::{Context, Event, FullOutput, Modifiers, Plugin, RawInput, ViewportId};

/// Installs the egui plugin that keeps input in order around accessibility
/// focus requests and records each pass's starting modifiers.
///
/// Call this once when creating the context (in eframe, from the creation
/// callback's `egui_ctx`). Installing it again has no effect.
/// [`RichTextEditor`](super::RichTextEditor) also installs it the first time it
/// is shown, taking effect on the next pass; installing it in an already
/// running context cannot recover the starting modifiers of that first pass.
///
/// egui applies an AccessKit focus request when its target widget is added, so
/// without this plugin, input that follows the request in the same batch can
/// reach whichever widget runs first. The plugin gives each focus request a
/// separate pass, so that input before and after it reaches the right widget,
/// rich editors and native egui widgets such as `TextEdit` alike, regardless of
/// draw order. Ordinary batches need no extra pass. If the context's pass limit
/// is reached, the remaining input waits ahead of newer input and the plugin
/// requests an immediate repaint, which the host must honor to deliver it,
/// including when it calls `begin_pass` and `end_pass` itself.
///
/// The recorded modifiers let the widget tell a plain-text paste
/// (Command/Ctrl+Shift+V) from a rich one even when the modifiers change later
/// in the same batch.
///
/// # Examples
///
/// ```
/// let ctx = egui::Context::default();
/// textloom::adapter::egui::install_input_routing(&ctx);
/// ```
pub fn install_input_routing(ctx: &Context) {
    if ctx.plugin_opt::<InputRouting>().is_none() {
        ctx.add_plugin(InputRouting::default());
    }
}

#[derive(Default)]
struct InputRouting {
    pending: HashMap<ViewportId, PendingInput>,
    passes: Vec<(ViewportId, Modifiers)>,
}

pub(super) fn modifiers_at_pass_start(ctx: &Context) -> Modifiers {
    let modifiers = ctx
        .plugin::<InputRouting>()
        .lock()
        .passes
        .last()
        .map(|&(_, modifiers)| modifiers);
    modifiers.unwrap_or_else(|| {
        // Automatic installation can miss this pass's input hook. The first
        // pass starts with no modifiers; a later installation can only use the
        // current state until its first hook. Install at context creation to
        // capture the starting state even on that first displayed pass.
        if ctx.cumulative_pass_nr() == 0 {
            Modifiers::NONE
        } else {
            ctx.input(|input| input.modifiers)
        }
    })
}

struct PendingInput {
    batches: VecDeque<InputBatch>,
    latest_focused: bool,
}

struct InputBatch {
    events: VecDeque<Event>,
    focused: bool,
}

fn requests_focus(event: &Event) -> bool {
    focus_request_target(event).is_some()
}

pub(super) fn focus_request_target(event: &Event) -> Option<egui::accesskit::NodeId> {
    match event {
        Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Focus,
            target_node,
            target_tree,
            data: None,
        }) if *target_tree == egui::accesskit::TreeId::ROOT => Some(*target_node),
        _ => None,
    }
}

impl Plugin for InputRouting {
    fn debug_name(&self) -> &'static str {
        "textloom input routing"
    }

    fn input_hook(&mut self, ctx: &Context, input: &mut RawInput) {
        // Read egui's previous state before it processes this routed segment.
        // This also preserves held modifiers across frames and nested viewports.
        let modifiers = ctx.input_for(input.viewport_id, |input| input.modifiers);
        self.passes.push((input.viewport_id, modifiers));
        if !self.pending.contains_key(&input.viewport_id)
            && !input.events.iter().any(requests_focus)
        {
            return;
        }

        let pending = self
            .pending
            .entry(input.viewport_id)
            .or_insert_with(|| PendingInput {
                batches: VecDeque::new(),
                latest_focused: input.focused,
            });
        // A later raw batch may contain a pointer press that egui resolves before
        // widgets run. Keep it separate so it cannot steal earlier queued text.
        // Preserve window focus, but keep current timestamps and geometry.
        if !input.events.is_empty() || input.focused != pending.latest_focused {
            pending.batches.push_back(InputBatch {
                events: mem::take(&mut input.events).into(),
                focused: input.focused,
            });
            pending.latest_focused = input.focused;
        }
        let batch = pending.batches.front_mut().expect(
            "pending viewports keep at least one batch, and a new entry receives this pass's \
             focus request",
        );
        input.focused = batch.focused;
        if batch.events.front().is_some_and(requests_focus) {
            input.events.push(
                batch
                    .events
                    .pop_front()
                    .expect("the front event was just checked"),
            );
        } else {
            while batch
                .events
                .front()
                .is_some_and(|event| !requests_focus(event))
            {
                input.events.push(
                    batch
                        .events
                        .pop_front()
                        .expect("the front event was just checked"),
                );
            }
        }
        if batch.events.is_empty() {
            pending.batches.pop_front();
        }
        if pending.batches.is_empty() {
            self.pending.remove(&input.viewport_id);
        }
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        // Automatic installation may happen after this pass's input hook. The
        // stack also identifies the ended viewport after a nested viewport pass.
        let Some((viewport, _)) = self.passes.pop() else {
            return;
        };
        if self.passes.is_empty() {
            self.pending
                .retain(|id, _| output.viewport_output.contains_key(id));
        }
        if self.pending.contains_key(&viewport) {
            output
                .platform_output
                .request_discard_reasons
                .push(egui::RepaintCause::new_reason(
                    "ordered accessibility focus",
                ));
            ctx.request_repaint_of(viewport);
            if let Some(output) = output.viewport_output.get_mut(&viewport) {
                output.repaint_delay = Duration::ZERO;
            }
        }
    }
}

#[cfg(test)]
mod tests;
