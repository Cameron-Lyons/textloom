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
/// The plugin also preserves the plain-text intent of replies to the widget's
/// [`egui::ViewportCommand::RequestPaste`]. Integrations must service that
/// command before delivering the next input batch, as eframe does. If the
/// clipboard is empty, that batch contains no reply; the request then expires
/// without changing the next unrelated paste.
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
    passes: Vec<InputPass>,
    paste_requests: HashMap<ViewportId, (u64, usize)>,
}

pub(super) fn modifiers_at_pass_start(ctx: &Context) -> Modifiers {
    let modifiers = ctx
        .plugin::<InputRouting>()
        .lock()
        .passes
        .last()
        .map(|pass| pass.modifiers);
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

/// Requests the stock integration's clipboard read. Its reply belongs to the
/// next input batch, even if that batch is split into several focus passes.
pub(super) fn request_plain_paste(ctx: &Context) {
    let viewport = ctx.viewport_id();
    let frame = ctx.cumulative_frame_nr();
    let plugin = ctx.plugin::<InputRouting>();
    let mut routing = plugin.lock();
    let request = routing.paste_requests.entry(viewport).or_insert((frame, 0));
    request.1 += 1;
    drop(routing);
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    ctx.request_repaint();
}

pub(super) fn is_plain_paste(ctx: &Context, paste_index: usize) -> bool {
    ctx.plugin::<InputRouting>()
        .lock()
        .passes
        .last()
        .and_then(|pass| pass.plain_pastes.get(paste_index))
        .copied()
        .unwrap_or(false)
}

struct InputPass {
    viewport: ViewportId,
    modifiers: Modifiers,
    plain_pastes: Vec<bool>,
}

struct RoutedEvent {
    event: Event,
    plain_paste: bool,
}

struct PendingInput {
    batches: VecDeque<InputBatch>,
    latest_focused: bool,
}

struct InputBatch {
    events: VecDeque<RoutedEvent>,
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
        let mut pass = InputPass {
            viewport: input.viewport_id,
            modifiers,
            plain_pastes: Vec::new(),
        };
        // Extra layout/focus passes run before the host can service output.
        // begin_pass/end_pass hosts have no frame counter, but start at pass 0.
        let new_batch = self
            .paste_requests
            .get(&input.viewport_id)
            .is_some_and(|&(frame, _)| {
                ctx.cumulative_frame_nr_for(input.viewport_id) > frame
                    || (ctx.viewport_id() == input.viewport_id && ctx.current_pass_index() == 0)
            });
        let mut plain_replies = if new_batch {
            self.paste_requests
                .remove(&input.viewport_id)
                .map_or(0, |(_, count)| count)
        } else {
            0
        };
        if !self.pending.contains_key(&input.viewport_id)
            && !input.events.iter().any(requests_focus)
        {
            pass.plain_pastes = input
                .events
                .iter()
                .filter(|event| matches!(event, Event::Paste(_)))
                .map(|_| {
                    let plain = plain_replies > 0;
                    plain_replies = plain_replies.saturating_sub(1);
                    plain
                })
                .collect();
            self.passes.push(pass);
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
                events: mem::take(&mut input.events)
                    .into_iter()
                    .map(|event| {
                        let plain_paste = matches!(event, Event::Paste(_)) && plain_replies > 0;
                        if plain_paste {
                            plain_replies -= 1;
                        }
                        RoutedEvent { event, plain_paste }
                    })
                    .collect(),
                focused: input.focused,
            });
            pending.latest_focused = input.focused;
        }
        let batch = pending.batches.front_mut().expect(
            "pending viewports keep at least one batch, and a new entry receives this pass's \
             focus request",
        );
        input.focused = batch.focused;
        if batch
            .events
            .front()
            .is_some_and(|event| requests_focus(&event.event))
        {
            input.events.push(
                batch
                    .events
                    .pop_front()
                    .expect("the front event was just checked")
                    .event,
            );
        } else {
            while batch
                .events
                .front()
                .is_some_and(|event| !requests_focus(&event.event))
            {
                let event = batch
                    .events
                    .pop_front()
                    .expect("the front event was just checked");
                if matches!(event.event, Event::Paste(_)) {
                    pass.plain_pastes.push(event.plain_paste);
                }
                input.events.push(event.event);
            }
        }
        if batch.events.is_empty() {
            pending.batches.pop_front();
        }
        if pending.batches.is_empty() {
            self.pending.remove(&input.viewport_id);
        }
        self.passes.push(pass);
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        // Automatic installation may happen after this pass's input hook. The
        // stack also identifies the ended viewport after a nested viewport pass.
        let Some(InputPass { viewport, .. }) = self.passes.pop() else {
            return;
        };
        if self.passes.is_empty() {
            self.pending
                .retain(|id, _| output.viewport_output.contains_key(id));
            self.paste_requests
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
