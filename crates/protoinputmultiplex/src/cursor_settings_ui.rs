//! Applied cursor policy is separate from the profile's editable draft.
use crate::session::{SettingsAcknowledgement, SettingsTicket};
use mux_core::{Seat, desktop::CursorSettings};
use serde::Serialize;
use std::collections::BTreeMap;

/// Reserve both real action rows on compact viewports; this same plan drives
/// the production footer and its offscreen control geometry regression.
pub fn footer_layout(
    available: eframe::egui::Rect,
    line_height: f32,
    gap: f32,
    conflict: bool,
) -> (eframe::egui::Rect, bool) {
    let separate = conflict && available.width() < 420.;
    let height = (line_height
        + ouroboros_ui::tokens::core::Size::Md.height() * if separate { 2. } else { 1. }
        + gap * if separate { 4. } else { 3. }
        + 1.)
        .min(available.height().max(0.));
    (
        eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(available.left(), available.bottom() - height),
            available.max,
        ),
        separate,
    )
}

pub fn footer_buttons(
    ui: &mut eframe::egui::Ui,
    apply_label: &str,
    can_apply: bool,
    conflict: bool,
    separate: bool,
    can_use_applied: bool,
    mut button: impl FnMut(&mut eframe::egui::Ui, &str, bool) -> bool,
) -> (bool, bool) {
    let mut apply = false;
    let mut use_applied = false;
    if conflict && separate {
        ui.horizontal(|ui| {
            ui.with_layout(
                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                |ui| {
                    use_applied = button(ui, "Use applied settings", can_use_applied);
                },
            );
        });
    }
    ui.horizontal(|ui| {
        ui.with_layout(
            eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
            |ui| {
                apply = button(ui, apply_label, can_apply);
                if conflict && !separate {
                    use_applied = button(ui, "Use applied settings", can_use_applied);
                }
            },
        );
    });
    (apply, use_applied)
}

struct Applied {
    settings: CursorSettings,
    external_change: bool,
}
struct Pending {
    index: usize,
    ticket: SettingsTicket,
    settings: CursorSettings,
}

#[derive(Default)]
pub struct CursorSettingsEditor {
    applied: BTreeMap<String, Applied>,
    pending: Option<Pending>,
    next_request: u64,
    error: Option<(String, String)>,
}

/// App-local evidence only; private request correlation never leaves the controller.
#[derive(Serialize)]
pub struct Snapshot {
    pub selected_identity: Option<String>,
    pub applied: Option<CursorSettings>,
    pub pending_identity: Option<String>,
    pub conflict: bool,
    pub error: Option<String>,
}

impl CursorSettingsEditor {
    pub fn reset(&mut self, seats: &[Seat]) {
        self.applied = seats
            .iter()
            .map(|seat| {
                (
                    seat.identity(),
                    Applied {
                        settings: seat.cursor.clone(),
                        external_change: false,
                    },
                )
            })
            .collect();
        self.pending = None;
        self.error = None;
        // Never recycle a correlation when a new controller/profile is installed.
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn keep_busy(&self, busy: bool) -> bool {
        busy || self.pending()
    }

    pub fn receive(
        &mut self,
        event: &crate::session::Event,
        seats: &mut [Seat],
    ) -> Option<Result<(), String>> {
        match event {
            crate::session::Event::SettingsAcknowledged(ack) => self.finish(ack, seats),
            _ => None,
        }
    }

    pub fn begin(&mut self, index: usize, seats: &[Seat]) -> Option<SettingsTicket> {
        if self.pending() {
            return None;
        }
        let seat = seats.get(index)?;
        self.next_request = self
            .next_request
            .checked_add(1)
            .expect("cursor request counter exhausted");
        let ticket = SettingsTicket {
            request: self.next_request,
            identity: seat.identity(),
        };
        self.pending = Some(Pending {
            index,
            ticket: ticket.clone(),
            settings: seat.cursor.clone(),
        });
        self.error = None;
        Some(ticket)
    }

    fn observe_one(&mut self, seat: &mut Seat, actual: &CursorSettings) {
        let identity = seat.identity();
        let entry = self
            .applied
            .entry(identity.clone())
            .or_insert_with(|| Applied {
                settings: seat.cursor.clone(),
                external_change: false,
            });
        if entry.settings != *actual {
            let clean = seat.cursor == entry.settings
                && self
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.ticket.identity != identity);
            entry.settings = actual.clone();
            if clean {
                seat.cursor = actual.clone();
                entry.external_change = false;
            } else {
                entry.external_change = seat.cursor != *actual;
            }
        }
    }

    pub fn observe_profile(&mut self, seats: &mut [Seat], actual: &[Seat]) {
        self.applied
            .retain(|identity, _| seats.iter().any(|seat| seat.identity() == *identity));
        for seat in seats {
            if let Some(source) = actual
                .iter()
                .find(|source| source.identity() == seat.identity())
            {
                self.observe_one(seat, &source.cursor);
            }
        }
    }

    /// Periodic keyboard state shares the same baseline, so it cannot make a
    /// previously clean full-settings draft appear dirty before an Agent snapshot.
    pub fn observe_keyboard(
        &mut self,
        seat: &mut Seat,
        keyboard: mux_core::keyboard::KeyboardSettings,
    ) {
        let Some(entry) = self.applied.get(&seat.identity()) else {
            return;
        };
        let mut actual = entry.settings.clone();
        actual.keyboard = keyboard;
        self.observe_one(seat, &actual);
    }

    pub fn finish(
        &mut self,
        ack: &SettingsAcknowledgement,
        seats: &mut [Seat],
    ) -> Option<Result<(), String>> {
        let pending = self.pending.as_ref()?;
        if ack.ticket != pending.ticket
            || ack.index != pending.index
            || ack.settings != pending.settings
        {
            return None;
        }
        let pending = self.pending.take().expect("matched pending cursor request");
        let result = if seats
            .get(ack.index)
            .is_none_or(|seat| seat.identity() != ack.ticket.identity)
        {
            Err("Cursor settings acknowledgement no longer matches this user. Draft kept.".into())
        } else if let Some(error) = &ack.error {
            Err(error.clone())
        } else {
            self.applied.insert(
                pending.ticket.identity.clone(),
                Applied {
                    settings: ack.settings.clone(),
                    external_change: false,
                },
            );
            // The draft may have been edited again while Apply was pending.
            // A successful ACK establishes applied state; it never overwrites it.
            Ok(())
        };
        self.error = result
            .as_ref()
            .err()
            .map(|error| (pending.ticket.identity, error.clone()));
        Some(result)
    }

    pub fn conflict(&self, seat: &Seat) -> bool {
        self.applied
            .get(&seat.identity())
            .is_some_and(|entry| entry.external_change && entry.settings != seat.cursor)
    }

    pub fn use_applied(&mut self, seat: &mut Seat) -> bool {
        if self.pending() {
            return false;
        }
        let Some(entry) = self.applied.get_mut(&seat.identity()) else {
            return false;
        };
        seat.cursor = entry.settings.clone();
        entry.external_change = false;
        self.error = None;
        true
    }

    pub fn snapshot(&self, seat: Option<&Seat>) -> Snapshot {
        let identity = seat.map(Seat::identity);
        Snapshot {
            selected_identity: identity.clone(),
            applied: identity
                .as_ref()
                .and_then(|identity| self.applied.get(identity))
                .map(|entry| entry.settings.clone()),
            pending_identity: self
                .pending
                .as_ref()
                .map(|pending| pending.ticket.identity.clone()),
            conflict: seat.is_some_and(|seat| self.conflict(seat)),
            error: self
                .error
                .as_ref()
                .filter(|(user, _)| Some(user) == identity.as_ref())
                .map(|(_, error)| error.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::desktop::CursorRenderer;

    fn seat(id: &str) -> Seat {
        serde_json::from_value(serde_json::json!({"id":id,"name":id,"mouse":"virtual://mouse/1"}))
            .unwrap()
    }
    fn ack(
        ticket: SettingsTicket,
        index: usize,
        settings: &CursorSettings,
    ) -> SettingsAcknowledgement {
        SettingsAcknowledgement {
            ticket,
            index,
            settings: settings.clone(),
            error: None,
        }
    }

    #[test]
    fn clean_external_sync_and_dirty_discard_use_latest_full_policy_by_identity() {
        let mut editor = CursorSettingsEditor::default();
        let mut seats = vec![seat("a"), seat("b")];
        editor.reset(&seats);
        let mut actual = seats.clone();
        actual[0].cursor.renderer = CursorRenderer::Gdi;
        editor.observe_profile(&mut seats, &actual);
        assert_eq!(seats[0].cursor, actual[0].cursor);
        assert!(!editor.conflict(&seats[0]));
        seats[0].cursor.speed_x = 2.;
        actual[0].cursor.renderer = CursorRenderer::Auto;
        actual[0].cursor.badge = Some("remote".into());
        actual.reverse();
        editor.observe_profile(&mut seats, &actual);
        assert_eq!(seats[0].cursor.speed_x, 2.);
        assert_eq!(seats[0].cursor.renderer, CursorRenderer::Gdi);
        assert!(editor.conflict(&seats[0]));
        assert_eq!(seats[1].cursor, actual[0].cursor);
        assert!(editor.use_applied(&mut seats[0]));
        assert_eq!(seats[0].cursor, actual[1].cursor);
        assert!(!editor.conflict(&seats[0]));
    }

    #[test]
    fn external_same_user_ack_and_stale_or_wrong_settings_cannot_finish_local_apply() {
        let mut editor = CursorSettingsEditor::default();
        let mut seats = vec![seat("a"), seat("b")];
        editor.reset(&seats);
        seats[0].cursor.size = 44.;
        let requested = seats[0].cursor.clone();
        let ticket = editor.begin(0, &seats).unwrap();
        let mut actual = seats.clone();
        actual[0].cursor.renderer = CursorRenderer::Gdi;
        actual[1].cursor.speed_x = 3.;
        editor.observe_profile(&mut seats, &actual);
        assert!(editor.pending() && editor.conflict(&seats[0]));
        assert_eq!(seats[1].cursor.speed_x, 3.);
        assert!(editor.begin(1, &seats).is_none());
        let mut unrelated = ack(ticket.clone(), 1, &requested);
        assert!(editor.finish(&unrelated, &mut seats).is_none());
        unrelated.index = 0;
        unrelated.settings.size = 45.;
        assert!(editor.finish(&unrelated, &mut seats).is_none());
        assert!(editor.pending());
        for event in [
            crate::session::Event::InputSent,
            crate::session::Event::CommandRejected("unrelated".into()),
            crate::session::Event::Agent(crate::agent_runtime::State::default()),
        ] {
            assert!(editor.receive(&event, &mut seats).is_none());
            assert!(editor.keep_busy(false));
        }
        let mut rejected = ack(ticket, 0, &requested);
        rejected.error = Some("held input".into());
        assert!(editor.finish(&rejected, &mut seats).unwrap().is_err());
        assert_eq!(seats[0].cursor, requested);
        assert!(editor.conflict(&seats[0]));
        assert!(editor.use_applied(&mut seats[0]));
        assert_eq!(seats[0].cursor.renderer, CursorRenderer::Gdi);
    }

    #[test]
    fn reset_reorder_and_post_submission_edits_do_not_adopt_stale_ack() {
        let mut editor = CursorSettingsEditor::default();
        let mut seats = vec![seat("a"), seat("b")];
        editor.reset(&seats);
        let old = editor.begin(0, &seats).unwrap();
        let old_ack = ack(old.clone(), 0, &seats[0].cursor);
        seats.swap(0, 1);
        assert!(editor.finish(&old_ack, &mut seats).unwrap().is_err());
        editor.reset(&seats);
        let current = editor.begin(0, &seats).unwrap();
        assert!(current.request > old.request);
        assert!(editor.finish(&old_ack, &mut seats).is_none());
        let accepted = ack(current, 0, &seats[0].cursor);
        seats[0].cursor.speed_y = 3.;
        assert!(editor.finish(&accepted, &mut seats).unwrap().is_ok());
        assert_eq!(seats[0].cursor.speed_y, 3.);
        assert_eq!(
            editor.snapshot(Some(&seats[0])).applied.unwrap().speed_y,
            1.
        );
        editor.reset(&[]);
        assert!(editor.snapshot(Some(&seats[0])).applied.is_none());
    }

    #[test]
    fn partial_keyboard_sample_preserves_whole_draft_and_clean_baseline() {
        let mut editor = CursorSettingsEditor::default();
        let mut seats = vec![seat("a")];
        editor.reset(&seats);
        let mut keyboard = seats[0].cursor.keyboard;
        keyboard.enabled = false;
        editor.observe_keyboard(&mut seats[0], keyboard);
        assert_eq!(seats[0].cursor.keyboard, keyboard);
        let mut actual = seats.clone();
        actual[0].cursor.renderer = CursorRenderer::Gdi;
        editor.observe_profile(&mut seats, &actual);
        assert!(!editor.conflict(&seats[0]));
        assert_eq!(seats[0].cursor.renderer, CursorRenderer::Gdi);
        seats[0].cursor.speed_x = 2.;
        keyboard.enabled = true;
        editor.observe_keyboard(&mut seats[0], keyboard);
        assert!(!seats[0].cursor.keyboard.enabled, "whole dirty draft kept");
        assert!(editor.conflict(&seats[0]));
        assert!(editor.use_applied(&mut seats[0]));
        assert!(seats[0].cursor.keyboard.enabled);
    }

    #[test]
    fn conflict_footer_real_buttons_stay_inside_compact_and_normal_content() {
        use eframe::egui;
        use ouroboros_ui::{
            Theme,
            atoms::{Button, Divider, Text},
        };
        for width in [288., 508.] {
            let ctx = egui::Context::default();
            let mut fonts = egui::FontDefinitions::default();
            ouroboros_ui::theme::typography::register(&mut fonts);
            ctx.set_fonts(fonts);
            Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut controls = Vec::new();
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width + 16., 180.),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        let (footer, separate) = footer_layout(
                            ui.available_rect_before_wrap(),
                            ui.text_style_height(&egui::TextStyle::Small),
                            ui.spacing().item_spacing.y,
                            true,
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(footer)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                ui.set_clip_rect(ui.clip_rect().intersect(footer));
                                Text::new("Applied settings changed; draft kept.")
                                    .caption()
                                    .show(ui);
                                Divider::horizontal().show(ui);
                                footer_buttons(
                                    ui,
                                    "Apply cursor settings",
                                    true,
                                    true,
                                    separate,
                                    true,
                                    |ui, label, enabled| {
                                        let response = Button::new(label).enabled(enabled).show(ui);
                                        controls.push((
                                            label.to_owned(),
                                            response.rect,
                                            ui.clip_rect(),
                                        ));
                                        response.clicked()
                                    },
                                );
                            },
                        );
                    });
                },
            );
            assert_eq!(controls.len(), 2);
            assert!(
                controls
                    .iter()
                    .all(|(_, rect, clip)| clip.contains_rect(*rect)),
                "{controls:?}"
            );
            assert!(!controls[0].1.intersects(controls[1].1));
        }
    }
}
