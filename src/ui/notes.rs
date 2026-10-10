//! The user's notes on servers: the editor in the details panel, the way to it from the list's
//! right-click menu, and the small mark drawn after a noted server's name in the list.

use eframe::egui::{self, RichText, Sense};

use super::App;

/// The longest note, in characters.
const NOTE_LIMIT: usize = 2000;

/// A note being edited in the details panel. It's saved when the editor loses focus, when
/// another server is selected, and when the browser closes.
pub(super) struct NoteDraft {
    addr: String,
    text: String,
    /// Put the cursor in the editor on the next frame.
    focus: bool,
}

impl App {
    /// Opens the note editor for a server, selecting it.
    pub(super) fn edit_note(&mut self, addr: &str) {
        self.commit_note();
        if self.selected.as_deref() != Some(addr) {
            self.select(addr, false);
        }
        self.note_draft = Some(NoteDraft {
            addr: addr.to_string(),
            text: self.state.notes.get(addr).cloned().unwrap_or_default(),
            focus: true,
        });
    }

    /// Saves the note being edited (an empty one removes it) and puts the editor away; it comes
    /// back by itself for a server with a note.
    pub(super) fn commit_note(&mut self) {
        let Some(draft) = self.note_draft.take() else {
            return;
        };
        if self.state.set_note(&draft.addr, &draft.text) {
            self.state_changed();
            // The search looks in notes too.
            if !self.state.filters.search.trim().is_empty() {
                self.view_dirty = true;
            }
        }
    }

    /// Saves the note of a server that's no longer selected.
    pub(super) fn note_follows_selection(&mut self) {
        if self
            .note_draft
            .as_ref()
            .is_some_and(|d| self.selected.as_deref() != Some(d.addr.as_str()))
        {
            self.commit_note();
        }
    }

    /// "Add note" (or "Edit note" for a server with one), the last of the details panel's
    /// buttons, so nothing moves under the pointer when it's clicked. It puts the cursor in the
    /// note.
    pub(super) fn note_button(&mut self, ui: &mut egui::Ui, addr: &str) {
        let editing = self.note_draft.as_ref().is_some_and(|d| d.addr == addr);
        let (label, hint) = if editing || self.state.notes.contains_key(addr) {
            ("Edit note", "Your note on this server is below")
        } else {
            (
                "Add note",
                "Write down something to remember about this server",
            )
        };
        if ui.button(label).on_hover_text(hint).clicked() {
            self.edit_note(addr);
        }
    }

    /// The note editor in the details panel, for a server with a note or one being written.
    pub(super) fn note_section(&mut self, ui: &mut egui::Ui, addr: &str) {
        if self.note_draft.is_none()
            && let Some(note) = self.state.notes.get(addr)
        {
            self.note_draft = Some(NoteDraft {
                addr: addr.to_string(),
                text: note.clone(),
                focus: false,
            });
        }
        let Some(draft) = self.note_draft.as_mut().filter(|d| d.addr == addr) else {
            return;
        };
        ui.add_space(8.0);
        ui.label(RichText::new("Note").strong());
        // A long note scrolls rather than pushing the mods out of the panel.
        let response = egui::ScrollArea::vertical()
            .id_salt("note")
            .max_height(80.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut draft.text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(NOTE_LIMIT)
                        .hint_text("Only you see this. Leave it empty to remove it."),
                )
            })
            .inner;
        // Asked for until the field has it: focus asked for in the frame the button was clicked
        // doesn't stay.
        if draft.focus {
            if response.has_focus() {
                draft.focus = false;
            } else {
                response.request_focus();
                // The details panel may be scrolled past it.
                response.scroll_to_me(None);
            }
        }
        if response.lost_focus() {
            self.commit_note();
        }
    }
}

/// How wide the note mark is.
pub(super) const NOTE_MARK_WIDTH: f32 = 10.0;

/// A small sheet of paper with a folded corner and two lines of writing, drawn rather than taken
/// from the emoji font, whose notepads are hard to make out at this size.
pub(super) fn note_mark(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(NOTE_MARK_WIDTH, 12.0), Sense::hover());
    let color = ui.visuals().weak_text_color();
    let stroke = egui::Stroke::new(1.0, color);
    let page = rect.shrink(0.5);
    let fold = 3.0;
    let painter = ui.painter();
    painter.add(egui::Shape::closed_line(
        vec![
            page.left_top(),
            egui::pos2(page.right() - fold, page.top()),
            egui::pos2(page.right(), page.top() + fold),
            page.right_bottom(),
            page.left_bottom(),
        ],
        stroke,
    ));
    painter.add(egui::Shape::line(
        vec![
            egui::pos2(page.right() - fold, page.top()),
            egui::pos2(page.right() - fold, page.top() + fold),
            egui::pos2(page.right(), page.top() + fold),
        ],
        stroke,
    ));
    for y in [page.top() + 5.5, page.top() + 8.5] {
        painter.hline(page.left() + 2.0..=page.right() - 2.0, y, stroke);
    }
    response
}
