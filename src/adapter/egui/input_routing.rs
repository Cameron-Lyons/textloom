use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use egui::{Context, Event, FullOutput, Plugin, RawInput, ViewportId};

/// Preserve input order around accessibility focus requests, including native
/// egui widgets in the same context.
///
/// Call this once when creating the context. [`super::RichTextEditor`] also
/// installs it automatically when first shown, taking effect on the next pass.
///
/// egui applies an accessibility focus request when its target widget is added.
/// This plugin gives each focus request a separate pass so that input before and
/// after it reaches the appropriate widget regardless of widget order. Ordinary
/// batches need no extra pass. If the context's pass limit is reached, remaining
/// input stays ordered ahead of new input and requests another repaint.
pub fn install_input_routing(ctx: &Context) {
    if ctx.plugin_opt::<InputRouting>().is_none() {
        ctx.add_plugin(InputRouting::default());
    }
}

#[derive(Default)]
struct InputRouting {
    pending: HashMap<ViewportId, PendingInput>,
    passes: Vec<ViewportId>,
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
    matches!(event, Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Focus,
        target_tree,
        data: None,
        ..
    }) if *target_tree == egui::accesskit::TreeId::ROOT)
}

impl Plugin for InputRouting {
    fn debug_name(&self) -> &'static str {
        "textloom input routing"
    }

    fn input_hook(&mut self, _: &Context, input: &mut RawInput) {
        self.passes.push(input.viewport_id);
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
                events: std::mem::take(&mut input.events).into(),
                focused: input.focused,
            });
            pending.latest_focused = input.focused;
        }
        let batch = pending.batches.front_mut().expect("pending input batch");
        input.focused = batch.focused;
        if batch.events.front().is_some_and(requests_focus) {
            input
                .events
                .push(batch.events.pop_front().expect("checked event"));
        } else {
            while batch
                .events
                .front()
                .is_some_and(|event| !requests_focus(event))
            {
                input
                    .events
                    .push(batch.events.pop_front().expect("checked event"));
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
        let Some(viewport) = self.passes.pop() else {
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
