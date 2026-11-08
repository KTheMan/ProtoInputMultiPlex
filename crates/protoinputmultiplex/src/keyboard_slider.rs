//! Keyboard and accessibility behavior for the app's pinned Ouroboros slider atom.

use eframe::egui::{self, Id, Key, Modifiers, Response, Ui};
use ouroboros_ui::atoms::Slider;
use std::hash::Hash;

/// Paint an Ouroboros slider and add focused keyboard and AccessKit value actions.
///
/// Keyboard steps are additive, so an existing value between steps is preserved until
/// the user changes it. The widget paints a bounded local copy, keeping invalid or
/// out-of-range persisted values untouched until an actual pointer or key edit occurs.
pub fn accessible_slider(
    ui: &mut Ui,
    id: impl Hash,
    accessible_name: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    step: f32,
) -> Response {
    assert!(min.is_finite() && max.is_finite() && min < max);
    assert!(step.is_finite() && step > 0.0);

    let enabled = ui.is_enabled();
    let original = *value;
    let mut painted_value = bounded_value(original, min, max);
    let response = ui
        .scope_builder(
            egui::UiBuilder::new().id(Id::new(("protoinputmultiplex-keyboard-slider", id))),
            |ui| {
                Slider::new(&mut painted_value)
                    .range(min, max)
                    .step(step)
                    .show(ui)
            },
        )
        .inner;
    let mut response = response;

    if response.changed() {
        *value = painted_value;
    }

    if enabled && response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
    }

    let (key_delta, absolute) = if enabled && response.has_focus() {
        let mut delta = 0_i64;
        let mut absolute = None;
        ui.input_mut(|input| {
            delta += input.count_and_consume_key(Modifiers::NONE, Key::ArrowRight) as i64;
            delta -= input.count_and_consume_key(Modifiers::NONE, Key::ArrowLeft) as i64;
            delta += input.count_and_consume_key(Modifiers::NONE, Key::ArrowUp) as i64;
            delta -= input.count_and_consume_key(Modifiers::NONE, Key::ArrowDown) as i64;
            delta += 10 * input.count_and_consume_key(Modifiers::NONE, Key::PageUp) as i64;
            delta -= 10 * input.count_and_consume_key(Modifiers::NONE, Key::PageDown) as i64;
            if input.consume_key(Modifiers::NONE, Key::Home) {
                absolute = Some(min);
            }
            if input.consume_key(Modifiers::NONE, Key::End) {
                absolute = Some(max);
            }
        });
        (delta, absolute)
    } else {
        (0, None)
    };

    let mut action_delta = 0_i64;
    let mut action_value = None;
    if enabled {
        ui.input_mut(|input| {
            input.consume_accesskit_action_requests(response.id, |request| {
                use egui::accesskit::{Action, ActionData};
                match request.action {
                    Action::Increment => {
                        action_delta += 1;
                        true
                    }
                    Action::Decrement => {
                        action_delta -= 1;
                        true
                    }
                    Action::SetValue => {
                        if let Some(ActionData::NumericValue(requested)) = &request.data
                            && requested.is_finite()
                            && let requested =
                                requested.clamp(f64::from(min), f64::from(max)) as f32
                            && requested.is_finite()
                        {
                            action_value = Some(requested.clamp(min, max));
                        }
                        true
                    }
                    _ => false,
                }
            });
        });
    }

    let current = if response.changed() {
        *value
    } else {
        bounded_value(original, min, max)
    };
    let next = action_value
        .or(absolute)
        .unwrap_or_else(|| current + (key_delta + action_delta) as f32 * step)
        .clamp(min, max);

    // Do not quantize values just because the widget rendered. Pointer updates are
    // already snapped by Ouroboros; keyboard and AccessKit changes preserve the
    // requested additive/value semantics, including both endpoints.
    if enabled
        && (key_delta != 0 || absolute.is_some() || action_delta != 0 || action_value.is_some())
        && next != *value
    {
        *value = next;
        response.mark_changed();
        ui.ctx().request_repaint();
    }

    let info_value = if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    };
    response
        .widget_info(|| egui::WidgetInfo::slider(enabled, f64::from(info_value), accessible_name));
    let _ = ui.ctx().accesskit_node_builder(response.id, |node| {
        use egui::accesskit::Action;
        node.set_min_numeric_value(f64::from(min));
        node.set_max_numeric_value(f64::from(max));
        node.set_numeric_value_step(f64::from(step));
        if enabled {
            node.add_action(Action::SetValue);
            if info_value < max {
                node.add_action(Action::Increment);
            }
            if info_value > min {
                node.add_action(Action::Decrement);
            }
        }
    });
    response
}

fn bounded_value(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

/// Set a native accessible label without changing the button's visible/script label.
pub fn set_accessible_label(ui: &Ui, response: &Response, label: &str) {
    let _ = ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(label.to_owned());
    });
}

/// Bound and flatten user-provided seat names before including them in accessibility text.
pub fn bounded_accessible_name(name: &str) -> String {
    let bounded: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    let normalized = bounded.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "Unnamed user".to_owned()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Id, Pos2, RawInput, Rect, Vec2};

    fn input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0))),
            events,
            ..RawInput::default()
        }
    }

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn frame(
        ctx: &egui::Context,
        value: &mut f32,
        events: Vec<Event>,
        request_focus: bool,
        disabled: bool,
        tab_target_focused: &mut bool,
    ) -> (Response, Id) {
        let mut response = None;
        let mut tab_id = Id::NULL;
        let _ = ctx.run_ui(input(events), |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                let slider = ui
                    .add_enabled_ui(!disabled, |ui| {
                        accessible_slider(
                            ui,
                            Id::new("test-speed"),
                            "Horizontal motion speed for Test User",
                            value,
                            0.1,
                            8.0,
                            0.1,
                        )
                    })
                    .inner;
                if request_focus {
                    slider.request_focus();
                }
                let button = ui.button("Apply");
                *tab_target_focused = button.has_focus();
                tab_id = button.id;
                response = Some(slider);
            });
        });
        (response.expect("slider rendered"), tab_id)
    }

    fn warm_focus(ctx: &egui::Context, value: &mut f32) {
        let mut tab_focused = false;
        frame(ctx, value, vec![], true, false, &mut tab_focused);
        frame(ctx, value, vec![], false, false, &mut tab_focused);
    }

    fn reparented_frame(
        ctx: &egui::Context,
        value: &mut f32,
        events: Vec<Event>,
        nested_parent: bool,
        request_focus: bool,
    ) -> Response {
        let mut response_out = None;
        let _ = ctx.run_ui(input(events), |root_ui| {
            egui::CentralPanel::default().show_inside(root_ui, |ui| {
                let response = if nested_parent {
                    ui.vertical(|ui| {
                        ui.label("Compact settings");
                        ui.horizontal(|ui| {
                            ui.label("Horizontal speed");
                            accessible_slider(
                                ui,
                                ("motion-speed-x", 2_usize),
                                "Horizontal motion speed for Test User",
                                value,
                                0.1,
                                8.0,
                                0.1,
                            )
                        })
                        .inner
                    })
                    .inner
                } else {
                    ui.horizontal(|ui| {
                        ui.label("Wide settings");
                        ui.vertical(|ui| {
                            ui.label("Horizontal speed");
                            accessible_slider(
                                ui,
                                ("motion-speed-x", 2_usize),
                                "Horizontal motion speed for Test User",
                                value,
                                0.1,
                                8.0,
                                0.1,
                            )
                        })
                        .inner
                    })
                    .inner
                };
                if request_focus {
                    response.request_focus();
                }
                response_out = Some(response);
            });
        });
        response_out.expect("slider rendered in the selected parent layout")
    }

    #[test]
    fn focused_keyboard_changes_preserve_steps_endpoints_and_bounds() {
        let ctx = egui::Context::default();
        let mut value = 1.05;
        warm_focus(&ctx, &mut value);

        let mut tab_focused = false;
        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![key(Key::ArrowRight)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(response.changed());
        assert!(response.has_focus(), "arrow adjustment moved focus away");
        assert!((value - 1.15).abs() < 0.0001, "value was {value}");

        frame(
            &ctx,
            &mut value,
            vec![key(Key::PageUp)],
            false,
            false,
            &mut tab_focused,
        );
        assert!((value - 2.15).abs() < 0.0001);
        frame(
            &ctx,
            &mut value,
            vec![key(Key::Home)],
            false,
            false,
            &mut tab_focused,
        );
        assert_eq!(value, 0.1);
        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![key(Key::ArrowLeft)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(!response.changed());
        assert_eq!(value, 0.1);
        frame(
            &ctx,
            &mut value,
            vec![key(Key::End)],
            false,
            false,
            &mut tab_focused,
        );
        assert_eq!(value, 8.0);
        frame(
            &ctx,
            &mut value,
            vec![key(Key::PageDown)],
            false,
            false,
            &mut tab_focused,
        );
        assert_eq!(value, 7.0);
    }

    #[test]
    fn unfocused_and_disabled_sliders_do_not_consume_or_apply_keys() {
        let ctx = egui::Context::default();
        let mut value = 1.0;
        let mut tab_focused = false;
        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![key(Key::ArrowRight)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(!response.changed());
        assert_eq!(value, 1.0);

        warm_focus(&ctx, &mut value);
        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![key(Key::ArrowRight)],
            false,
            true,
            &mut tab_focused,
        );
        assert!(!response.enabled());
        assert!(!response.changed());
        assert_eq!(value, 1.0);
    }

    #[test]
    fn tab_moves_from_slider_to_apply_button() {
        let ctx = egui::Context::default();
        let mut value = 1.0;
        let mut tab_focused = false;
        frame(&ctx, &mut value, vec![], true, false, &mut tab_focused);
        frame(
            &ctx,
            &mut value,
            vec![key(Key::Tab)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(tab_focused);
        assert_eq!(value, 1.0);
    }

    #[test]
    fn accesskit_actions_route_through_the_same_bounds_without_nonfinite_corruption() {
        let ctx = egui::Context::default();
        let mut value = 3.0;
        let mut tab_focused = false;
        let (slider, _) = frame(&ctx, &mut value, vec![], true, false, &mut tab_focused);
        let action = |action, data| {
            Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: slider.id.accesskit_id(),
                data,
            })
        };
        let set_value = |value| {
            action(
                egui::accesskit::Action::SetValue,
                Some(egui::accesskit::ActionData::NumericValue(value)),
            )
        };

        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![set_value(6.25)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(response.changed());
        assert_eq!(value, 6.25);

        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![action(egui::accesskit::Action::Increment, None)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(response.changed());
        assert!((value - 6.35).abs() < 0.0001);

        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![action(egui::accesskit::Action::Decrement, None)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(response.changed());
        assert!((value - 6.25).abs() < 0.0001);

        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![set_value(f64::MAX)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(response.changed());
        assert_eq!(value, 8.0);

        frame(
            &ctx,
            &mut value,
            vec![set_value(6.25)],
            false,
            false,
            &mut tab_focused,
        );
        assert_eq!(value, 6.25);

        let (response, _) = frame(
            &ctx,
            &mut value,
            vec![set_value(f64::NAN)],
            false,
            false,
            &mut tab_focused,
        );
        assert!(!response.changed());
        assert_eq!(value, 6.25);
    }

    #[test]
    fn rendering_preserves_loaded_off_grid_and_nonfinite_values() {
        let ctx = egui::Context::default();
        let mut tab_focused = false;
        let mut off_grid = 1.05;
        let (response, _) = frame(&ctx, &mut off_grid, vec![], false, false, &mut tab_focused);
        assert!(!response.changed());
        assert_eq!(off_grid, 1.05);

        let mut not_a_number = f32::NAN;
        frame(
            &ctx,
            &mut not_a_number,
            vec![],
            false,
            false,
            &mut tab_focused,
        );
        assert!(not_a_number.is_nan());
    }

    #[test]
    fn explicit_widget_identity_preserves_focus_and_actions_when_layout_reparents_slider() {
        let ctx = egui::Context::default();
        let mut value = 1.0;
        let first = reparented_frame(&ctx, &mut value, vec![], false, true);
        let original_id = first.id;
        reparented_frame(&ctx, &mut value, vec![], false, false);

        let moved = reparented_frame(&ctx, &mut value, vec![], true, false);
        assert_eq!(moved.id, original_id);
        assert!(moved.has_focus());

        let adjusted = reparented_frame(&ctx, &mut value, vec![key(Key::ArrowRight)], true, false);
        assert_eq!(adjusted.id, original_id);
        assert!(adjusted.has_focus());
        assert!(adjusted.changed());
        assert!((value - 1.1).abs() < 0.0001);

        let action = Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Increment,
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: original_id.accesskit_id(),
            data: None,
        });
        let acted = reparented_frame(&ctx, &mut value, vec![action], false, false);
        assert_eq!(acted.id, original_id);
        assert!(acted.changed());
        assert!((value - 1.2).abs() < 0.0001);
    }
}
