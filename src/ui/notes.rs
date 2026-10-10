//! The user's notes on servers: the editor in the details panel, the way to it from the list's
//! right-click menu, and the small mark after a noted server's name in the list.

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

    /// "Add note", beside Join and Favorite, for a server without one.
    pub(super) fn note_button(&mut self, ui: &mut egui::Ui, addr: &str) {
        let editing = self.note_draft.as_ref().is_some_and(|d| d.addr == addr);
        if !editing
            && !self.state.notes.contains_key(addr)
            && ui
                .button("Add note")
                .on_hover_text("Write down something to remember about this server")
                .clicked()
        {
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
        if std::mem::take(&mut draft.focus) {
            response.request_focus();
        }
        if response.lost_focus() {
            self.commit_note();
        }
    }
}

/// A server's name in the list followed by a small note mark; hovering either shows the note.
/// A long name is cut short so the mark stays in view.
pub(super) fn noted_name(ui: &mut egui::Ui, name: &str, note: &str) {
    let mark_width = 10.0 + ui.spacing().item_spacing.x;
    let room = (ui.available_width() - mark_width).max(0.0);
    let label = ui
        .allocate_ui(egui::vec2(room, ui.available_height()), |ui| {
            ui.add(
                egui::Label::new(name)
                    .truncate()
                    .show_tooltip_when_elided(false),
            )
        })
        .inner;
    let mark = note_mark(ui);
    label.union(mark).on_hover_text(note);
}

/// A small sheet of paper with a folded corner and two lines of writing, drawn rather than taken
/// from the emoji font, whose notepads are hard to make out at this size.
fn note_mark(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 12.0), Sense::hover());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A long name in a narrow column still leaves room for the mark.
    #[test]
    fn draws_noted_name() {
        let ctx = egui::Context::default();
        for width in [40.0, 300.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width + 50.0, 100.0),
                )),
                ..Default::default()
            };
            let mut used = 0.0;
            let mut output = ctx.run_ui(input, |ui| {
                ui.allocate_ui(egui::vec2(width, 22.0), |ui| {
                    ui.horizontal(|ui| {
                        noted_name(ui, &"A very long server name ".repeat(5), "A note");
                        used = ui.min_rect().width();
                    });
                });
            });
            // Fonts would go to the GPU here.
            output.textures_delta.clear();
            assert!(used <= width + 1.0, "{used} > {width}");
        }
    }
}
