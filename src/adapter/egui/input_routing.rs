use std::collections::{HashMap, VecDeque};
use std::mem;
use std::time::Duration;

use egui::{Context, Event, FullOutput, Id, Modifiers, Plugin, RawInput, Ui, ViewportId};

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
/// [`egui::ViewportCommand::RequestPaste`] for the requesting widget. Replies
/// may arrive in either of the next **two host input batches** in that viewport.
/// eframe 0.37's glow and wgpu backends drain clipboard actions before queuing
/// new viewport commands, so their reply arrives in the second batch. Extra
/// layout/focus passes inside `Context::run_ui` do not count as host batches.
/// With manual `begin_pass`/`end_pass`, each pair counts as a host batch.
/// The plugin requests repaint until this window closes; the host must honor it.
///
/// egui supplies neither request IDs nor empty-clipboard acknowledgments. The
/// integration contract is therefore bounded and FIFO: the next paste events
/// in those two batches are replies, in request order. Missing replies expire
/// after the second batch. Hosts must not interleave unrelated pastes while a
/// request is outstanding or deliver replies later; those cases cannot be
/// distinguished using egui's events. After expiry ordinary pastes are unaffected.
///
/// A reply is discarded if its widget loses focus before delivery, including
/// when an ordered AccessKit focus action targets a native `TextEdit`. Pointer
/// presses, touch starts, Tab, Escape, and window focus loss conservatively
/// cancel outstanding requests. Application code that changes focus during
/// widget construction must do so before input routing (before starting the
/// pass) to protect native widgets drawn before the requesting editor.
/// Canceled text replies remain empty `Event::Paste` events, which both editors
/// ignore. This preserves the order and count of paste events for integrations
/// that capture native clipboard data before routing and consume a capture
/// slot for each paste delivered to the UI, even when its text is empty.
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
    paste_requests: HashMap<ViewportId, VecDeque<PasteRequest>>,
    focused: HashMap<ViewportId, Option<Id>>,
    memory_viewport: Option<ViewportId>,
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

/// Requests a clipboard read owned by this widget, with a two-host-batch bound.
pub(super) fn request_plain_paste(ctx: &Context, owner: Id) {
    let viewport = ctx.viewport_id();
    let plugin = ctx.plugin::<InputRouting>();
    let mut routing = plugin.lock();
    routing.focused.insert(viewport, Some(owner));
    routing
        .paste_requests
        .entry(viewport)
        .or_default()
        .push_back(PasteRequest {
            batches_left: 2,
            reply: PasteReply::Plain(owner),
        });
    drop(routing);
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    ctx.request_repaint();
}

pub(super) fn paste_reply(ctx: &Context, paste_index: usize) -> PasteReply {
    ctx.plugin::<InputRouting>()
        .lock()
        .passes
        .last()
        .and_then(|pass| pass.paste_replies.get(paste_index))
        .copied()
        .unwrap_or_default()
}

pub(super) fn observe_widget_focus(ctx: &Context) {
    let focused = ctx.memory(|memory| memory.focused());
    ctx.plugin::<InputRouting>()
        .lock()
        .observe_focus(ctx.viewport_id(), focused);
}

struct PasteRequest {
    batches_left: u8,
    reply: PasteReply,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum PasteReply {
    #[default]
    Ordinary,
    Plain(Id),
    Discard,
}

impl PasteReply {
    fn observe_focus(&mut self, focused: Option<Id>) {
        if matches!(*self, Self::Plain(owner) if Some(owner) != focused) {
            *self = Self::Discard;
        }
    }
}

struct InputPass {
    viewport: ViewportId,
    modifiers: Modifiers,
    paste_replies: Vec<PasteReply>,
}

struct RoutedEvent {
    event: Event,
    paste_reply: PasteReply,
}

impl RoutedEvent {
    fn deliver(mut self, pass: &mut InputPass, input: &mut RawInput) {
        if let Event::Paste(text) = &mut self.event {
            if self.paste_reply == PasteReply::Discard {
                // Preserve the host's raw-capture/pass-delivery alignment.
                // Both RichTextEditor and egui TextEdit ignore empty pastes,
                // including when their destination has a nonempty selection.
                text.clear();
            }
            pass.paste_replies.push(self.paste_reply);
        } else if self.paste_reply == PasteReply::Discard {
            return; // A requested image has no text-paste capture slot.
        }
        input.events.push(self.event);
    }
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

fn cancels_paste(event: &Event) -> bool {
    matches!(
        event,
        Event::WindowFocused(false)
            | Event::PointerButton { pressed: true, .. }
            | Event::Touch {
                phase: egui::TouchPhase::Start,
                ..
            }
            | Event::Key {
                key: egui::Key::Tab | egui::Key::Escape,
                pressed: true,
                ..
            }
    )
}

impl InputRouting {
    fn observe_focus(&mut self, viewport: ViewportId, focused: Option<Id>) {
        self.focused.insert(viewport, focused);
        if let Some(requests) = self.paste_requests.get_mut(&viewport) {
            for request in requests {
                request.reply.observe_focus(focused);
            }
        }
        if let Some(pending) = self.pending.get_mut(&viewport) {
            for batch in &mut pending.batches {
                for event in &mut batch.events {
                    event.paste_reply.observe_focus(focused);
                }
            }
        }
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
            paste_replies: Vec::new(),
        };
        let focused =
            if self.memory_viewport.unwrap_or_else(|| ctx.viewport_id()) == input.viewport_id {
                ctx.memory(|memory| memory.focused())
            } else {
                self.focused.get(&input.viewport_id).copied().flatten()
            };
        self.observe_focus(input.viewport_id, focused);
        self.memory_viewport = Some(input.viewport_id);
        // Extra layout/focus passes run before the host can service output.
        // begin_pass/end_pass hosts have no frame counter, but start at pass 0.
        let first_pass = ctx.viewport_for(input.viewport_id, |viewport| {
            viewport.output.num_completed_passes == 0
        });
        let cancel = !input.focused || input.events.iter().any(cancels_paste);
        let mut eligible = 0;
        if let Some(requests) = self.paste_requests.get_mut(&input.viewport_id) {
            for request in requests {
                if cancel {
                    request.reply = PasteReply::Discard;
                }
                if first_pass {
                    request.batches_left -= 1;
                    eligible += 1;
                }
            }
        }
        let events: VecDeque<_> = mem::take(&mut input.events)
            .into_iter()
            .map(|event| {
                let paste_reply =
                    if matches!(event, Event::Paste(_) | Event::PasteImage(_)) && eligible > 0 {
                        eligible -= 1;
                        let reply = self
                            .paste_requests
                            .get_mut(&input.viewport_id)
                            .and_then(VecDeque::pop_front)
                            .expect("eligible counts outstanding requests")
                            .reply;
                        // This text-only editor must not forward a requested image
                        // to a different widget, or let it consume a later text paste.
                        if matches!(event, Event::PasteImage(_)) {
                            PasteReply::Discard
                        } else {
                            reply
                        }
                    } else {
                        PasteReply::Ordinary
                    };
                RoutedEvent { event, paste_reply }
            })
            .collect();
        if let Some(requests) = self.paste_requests.get_mut(&input.viewport_id) {
            requests.retain(|request| request.batches_left > 0);
            if requests.is_empty() {
                self.paste_requests.remove(&input.viewport_id);
            }
        }

        if !self.pending.contains_key(&input.viewport_id)
            && !events.iter().any(|event| requests_focus(&event.event))
        {
            for event in events {
                event.deliver(&mut pass, input);
            }
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
        if !events.is_empty() || input.focused != pending.latest_focused {
            pending.batches.push_back(InputBatch {
                events,
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
                event.deliver(&mut pass, input);
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

    fn on_end_pass(&mut self, ui: &mut Ui) {
        // Nested viewports restore their parent's memory before output_hook.
        self.observe_focus(ui.ctx().viewport_id(), ui.memory(|memory| memory.focused()));
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        // Automatic installation may happen after this pass's input hook. The
        // stack also identifies the ended viewport after a nested viewport pass.
        let Some(InputPass { viewport, .. }) = self.passes.pop() else {
            return;
        };
        self.memory_viewport = Some(self.passes.last().map_or(viewport, |pass| pass.viewport));
        if self.memory_viewport == Some(viewport) {
            // Also runs for manual begin_pass/end_pass integrations, after
            // egui has applied traversal and forgotten disappeared widgets.
            self.observe_focus(viewport, ctx.memory(|memory| memory.focused()));
        }
        if self.passes.is_empty() {
            self.pending
                .retain(|id, _| output.viewport_output.contains_key(id));
            self.paste_requests
                .retain(|id, _| output.viewport_output.contains_key(id));
            self.focused
                .retain(|id, _| output.viewport_output.contains_key(id));
        }
        if self.pending.contains_key(&viewport) {
            output
                .platform_output
                .request_discard_reasons
                .push(egui::RepaintCause::new_reason(
                    "ordered accessibility focus",
                ));
        }
        if self.pending.contains_key(&viewport) || self.paste_requests.contains_key(&viewport) {
            ctx.request_repaint_of(viewport);
            if let Some(output) = output.viewport_output.get_mut(&viewport) {
                output.repaint_delay = Duration::ZERO;
            }
        }
    }
}

#[cfg(test)]
mod tests;
