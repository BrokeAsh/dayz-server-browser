//! What the browser's windows (Join, Connect by address, Settings, and the rest) share: where
//! they first open, and Escape closing the frontmost one.

use eframe::egui::{self, Id, LayerId, Order};

/// How far below the top of the browser's window the windows first open: clear of the top bar
/// and its buttons.
const TOP: f32 = 64.0;
/// How far each window opened while others are open is moved down and right of them.
const CASCADE: f32 = 24.0;

/// The open windows, for Escape. Kept in egui's memory between frames.
#[derive(Clone, Default)]
struct Open {
    /// The windows shown last frame, frontmost last.
    stack: Vec<Id>,
    /// The windows shown so far this frame.
    shown: Vec<Id>,
    /// The window Escape closes this frame.
    closing: Option<Id>,
    /// Where the widget with the keyboard focus was at the end of the last frame.
    focus: Option<LayerId>,
}

fn with_open<R>(ctx: &egui::Context, f: impl FnOnce(&mut Open) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Open>(Id::new("open windows"))))
}

/// A window, opening (the first time) centred across the browser and below the top bar, or a
/// little below and right of the windows already open. `width` is its default width.
pub(super) fn placed<'a>(
    window: egui::Window<'a>,
    ctx: &egui::Context,
    id: Id,
    width: f32,
) -> egui::Window<'a> {
    let others = with_open(ctx, |open| open.stack.iter().filter(|w| **w != id).count());
    let area = ctx.content_rect();
    let step = CASCADE * others as f32;
    let x = (area.center().x - width / 2.0).max(area.left() + 8.0) + step;
    window
        .id(id)
        .default_pos(egui::pos2(x, area.top() + TOP + step))
}

/// At the start of each frame: works out whether Escape closes a window this frame, and which.
/// It doesn't while a menu or list is open (Escape closes that), while a text field outside the
/// frontmost window has the focus (Escape leaves the field), or while `busy` (Escape is wanted
/// for something else, such as cancelling the choice of a hotkey).
pub(super) fn begin_frame(ctx: &egui::Context, busy: bool) {
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape) && i.modifiers.is_none());
    let menu = egui::Popup::is_any_open(ctx);
    with_open(ctx, |open| {
        let shown = std::mem::take(&mut open.shown);
        open.stack.retain(|id| shown.contains(id));
        let front = open.stack.last().copied();
        let typing_elsewhere = open
            .focus
            .is_some_and(|layer| front.is_none_or(|id| layer != LayerId::new(Order::Middle, id)));
        open.closing = front.filter(|_| escape && !menu && !busy && !typing_elsewhere);
    });
}

/// At the end of each frame: notes where the keyboard focus is.
pub(super) fn end_frame(ctx: &egui::Context) {
    let focus = ctx
        .memory(|m| m.focused())
        .and_then(|id| ctx.read_response(id))
        .map(|r| r.layer_id);
    with_open(ctx, |open| open.focus = focus);
}

/// Each frame a window is shown, after showing it: notes that it's open (frontmost if it just
/// opened or was clicked, as egui puts it), and says whether Escape closes it this frame.
pub(super) fn escape_closes(ctx: &egui::Context, id: Id) -> bool {
    let layer = LayerId::new(Order::Middle, id);
    let clicked = ctx
        .input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        })
        .is_some_and(|pos| ctx.layer_id_at(pos) == Some(layer));
    with_open(ctx, |open| {
        open.shown.push(id);
        if clicked || !open.stack.contains(&id) {
            open.stack.retain(|w| *w != id);
            open.stack.push(id);
        }
        open.closing == Some(id)
    })
}

/// Brings a window that's open to the front.
pub(super) fn to_front(ctx: &egui::Context, id: Id) {
    ctx.move_to_top(LayerId::new(Order::Middle, id));
    with_open(ctx, |open| {
        if open.stack.contains(&id) {
            open.stack.retain(|w| *w != id);
            open.stack.push(id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, escape: bool, windows: &[&str]) -> Vec<String> {
        let mut input = egui::RawInput::default();
        if escape {
            input.events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let mut closed = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            begin_frame(&ctx, false);
            for name in windows {
                let id = Id::new(*name);
                placed(egui::Window::new(*name), &ctx, id, 200.0).show(&ctx, |ui| {
                    ui.label("Hello");
                });
                if escape_closes(&ctx, id) {
                    closed.push(name.to_string());
                }
            }
            end_frame(&ctx);
        });
        output.textures_delta.clear();
        closed
    }

    /// Escape closes the window opened last, then the one under it.
    #[test]
    fn escape_closes_the_frontmost_window() {
        let ctx = egui::Context::default();
        assert!(frame(&ctx, false, &["a"]).is_empty());
        assert!(frame(&ctx, false, &["a", "b"]).is_empty());
        assert!(frame(&ctx, false, &["a", "b"]).is_empty());
        assert_eq!(frame(&ctx, true, &["a", "b"]), ["b"]);
        assert!(frame(&ctx, false, &["a"]).is_empty());
        assert_eq!(frame(&ctx, true, &["a"]), ["a"]);
        assert!(frame(&ctx, true, &[]).is_empty());
    }
}
