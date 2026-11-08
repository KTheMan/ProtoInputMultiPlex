//! Explicit opt-in UI validation. Events enter egui only, never Windows SendInput.
use crate::gui_capture::{CaptureWorker, Ticket};
use crate::gui_script_io::ScriptIo;
use anyhow::Result;
use eframe::egui::{self, Pos2};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

#[derive(Deserialize)]
struct Step {
    button: String,
    wait_ms: u64,
    #[serde(default)]
    point: Option<[i32; 2]>,
    #[serde(default)]
    user: usize,
    #[serde(default)]
    wait_for_sample: bool,
    /// Observe a visible disabled response without submitting any pointer event.
    #[serde(default)]
    expect_disabled: bool,
    /// Require the entire response to be visible before staging local input.
    #[serde(default)]
    whole_response: bool,
    /// Opt-in local key input. Tab seeks this response through normal focus traversal.
    #[serde(default)]
    key: Option<ScriptKey>,
    #[serde(default)]
    fields: BTreeMap<String, String>,
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };

    fn draw_test_context() -> egui::Context {
        let context = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        ouroboros_ui::theme::typography::register(&mut fonts);
        context.set_fonts(fonts);
        ouroboros_ui::Theme::install(&context, ouroboros_ui::Mode::Dark);
        context
    }

    struct Fixture {
        directory: PathBuf,
        script: Script,
    }
    impl Fixture {
        fn new(steps: serde_json::Value) -> Self {
            Self::with_screenshots(steps, true)
        }
        fn with_screenshots(steps: serde_json::Value, screenshots: bool) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let directory = fresh_fixture_directory(&std::env::temp_dir(), &NEXT);
            let path = directory.join("ui.json");
            std::fs::write(&path, steps.to_string()).unwrap();
            let script = Script::load_with_guards(path, None, screenshots).unwrap();
            Self { directory, script }
        }
        fn settle_events(&self) {
            self.script.script_io.wait_for(|status| {
                status.fault.is_some() || status.acked_sequence == status.submitted_sequence
            });
            assert!(self.script.io_fault().is_none());
        }
    }
    // Earlier test binaries may leave evidence behind and Windows can reuse
    // their process ID. Never reopen or remove an existing fixture directory.
    fn fresh_fixture_directory(parent: &std::path::Path, next: &AtomicU64) -> PathBuf {
        for _ in 0..1024 {
            let directory = parent.join(format!(
                "protoinputmultiplex-keyboard-script-{}-{}",
                std::process::id(),
                next.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => return directory,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Could not create script fixture: {error}"),
            }
        }
        panic!("Script fixture directory names exhausted");
    }

    #[test]
    fn fixture_directory_collision_preserves_previous_evidence() {
        let fixture = Fixture::new(serde_json::json!([]));
        let previous = fixture.directory.join(format!(
            "protoinputmultiplex-keyboard-script-{}-1",
            std::process::id()
        ));
        std::fs::create_dir(&previous).unwrap();
        let sentinel = previous.join("previous-evidence.txt");
        std::fs::write(&sentinel, b"preserved").unwrap();
        let next = AtomicU64::new(1);
        let fresh = fresh_fixture_directory(&fixture.directory, &next);
        assert_ne!(fresh, previous);
        assert!(fresh.is_dir());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserved");
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                self.settle_events();
            }
            let _ = std::fs::remove_file(self.directory.join("ui.json"));
        }
    }

    #[test]
    fn actual_script_capture_filters_tickets_and_reports_receipt_rejection_and_retirement() {
        let mut fixture = Fixture::new(serde_json::json!([]));
        let probe = crate::gui_activity::Probe::new();
        let observer = probe.observer();
        assert!(observer.snapshot().render_capture.is_none());
        fixture.script.bind_capture_activity(&probe);
        let context = egui::Context::default();
        let viewport = egui::ViewportId::ROOT;
        let base = fixture.script.last_capture + Duration::from_secs(4);
        fixture
            .script
            .capture_at(&context, &egui::RawInput::default(), base);
        let capture = observer.snapshot().render_capture.unwrap();
        assert!(capture.enabled);
        assert_eq!(
            (
                capture.requested_count,
                capture.requested_generation,
                capture.received_generation
            ),
            (1, 1, 0)
        );
        assert!(capture.pending_age_ms.is_some());
        let valid = Ticket {
            script_id: fixture.script.capture_script_id,
            viewport_id: viewport,
            generation: 1,
            requested_at: SystemTime::now(),
        };
        let other = egui::ViewportId::from_hash_of("other-capture-viewport");
        let input = |ticket: Ticket, event_viewport: egui::ViewportId, size: [usize; 2]| {
            let image = std::sync::Arc::new(egui::ColorImage::new(
                size,
                vec![egui::Color32::RED; size[0] * size[1]],
            ));
            egui::RawInput {
                events: vec![egui::Event::Screenshot {
                    viewport_id: event_viewport,
                    user_data: egui::UserData::new(ticket),
                    image,
                }],
                ..Default::default()
            }
        };
        for (ticket, event_viewport) in [
            (
                Ticket {
                    script_id: valid.script_id + 1,
                    ..valid
                },
                viewport,
            ),
            (
                Ticket {
                    viewport_id: other,
                    ..valid
                },
                viewport,
            ),
            (valid, other),
            (
                Ticket {
                    generation: 0,
                    ..valid
                },
                viewport,
            ),
            (
                Ticket {
                    generation: 2,
                    ..valid
                },
                viewport,
            ),
        ] {
            fixture.script.capture_at(
                &context,
                &input(ticket, event_viewport, [2, 3]),
                base + Duration::from_millis(1),
            );
        }
        assert_eq!(
            observer.snapshot().render_capture.unwrap().received_count,
            0
        );
        fixture.script.capture_at(
            &context,
            &input(valid, viewport, [2, 3]),
            base + Duration::from_millis(2),
        );
        let received = observer.snapshot().render_capture.unwrap();
        assert_eq!(
            (received.received_count, received.received_generation),
            (1, 1)
        );
        assert_eq!(received.received_dimensions, Some([2, 3]));
        assert_eq!(received.worker_accepted, Some(true));
        assert_eq!(received.pending_age_ms, None);
        fixture.script.capture_at(
            &context,
            &input(valid, viewport, [1, 1]),
            base + Duration::from_millis(3),
        );
        assert_eq!(
            observer
                .snapshot()
                .render_capture
                .unwrap()
                .received_dimensions,
            Some([2, 3])
        );
        fixture.script.capture_at(
            &context,
            &egui::RawInput::default(),
            base + Duration::from_secs(4),
        );
        fixture.script.capture_at(
            &context,
            &input(valid, viewport, [1, 1]),
            base + Duration::from_secs(4),
        );
        let pending = observer.snapshot().render_capture.unwrap();
        assert_eq!(pending.requested_generation, 2);
        assert!(pending.pending_age_ms.is_some());
        assert_eq!(pending.received_count, 1);
        fixture.script.capture_at(
            &context,
            &input(
                Ticket {
                    generation: 2,
                    ..valid
                },
                viewport,
                [0, 3],
            ),
            base + Duration::from_secs(4),
        );
        let rejected = observer.snapshot().render_capture.unwrap();
        assert_eq!(rejected.received_generation, 2);
        assert_eq!(rejected.received_dimensions, Some([0, 3]));
        assert_eq!(rejected.worker_accepted, Some(false));
        assert_eq!(rejected.pending_age_ms, None);
        fixture.script.capture_at(
            &context,
            &egui::RawInput::default(),
            base + Duration::from_secs(8),
        );
        assert!(
            observer
                .snapshot()
                .render_capture
                .unwrap()
                .pending_age_ms
                .is_some()
        );
        drop(fixture);
        let retired = observer.snapshot().render_capture.unwrap();
        assert!(retired.retired && retired.pending_age_ms.is_none());
        let mut disabled = Fixture::with_screenshots(serde_json::json!([]), false);
        disabled.script.bind_capture_activity(&probe);
        disabled.script.capture_at(
            &context,
            &egui::RawInput::default(),
            base + Duration::from_secs(20),
        );
        let metadata = observer.snapshot().render_capture.unwrap();
        assert!(!metadata.enabled && !metadata.retired);
        assert_ne!(metadata.script_id, valid.script_id);
        assert_eq!(metadata.requested_generation, 0);
        assert!(metadata.publication.is_none());
        drop(disabled);
        assert!(observer.snapshot().render_capture.unwrap().retired);
    }

    #[test]
    fn user_pause_state_and_positive_negative_ack_roundtrip_with_exact_paths() {
        let fixture = Fixture::new(serde_json::json!([]));
        let ticket = crate::session::SettingsTicket {
            request: 7,
            identity: "a".into(),
        };
        let positive = crate::routing_engine::UserPauseState {
            user: 0,
            supported: true,
            paused: true,
            ..Default::default()
        };
        let negative = crate::routing_engine::UserPauseState {
            pending_releases: 1,
            error: Some("Saved release pending".into()),
            ..positive.clone()
        };
        let snapshot =
            serde_json::json!({"selected_user":0,"users":[positive.clone()],"pending":null});
        fixture.script.user_pause_state(&snapshot);
        fixture.script.user_pause_ack(&ticket, &positive);
        fixture.script.user_pause_ack(&ticket, &negative);
        fixture.script.wait_for_io();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.directory.join("ui.user-pause.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved, snapshot);
        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(fixture.directory.join("ui.user-pause.acks.jsonl"))
                .unwrap()
                .lines()
                .map(|row| serde_json::from_str(row).unwrap())
                .collect();
        assert_eq!(
            rows,
            vec![
                serde_json::json!({"ticket":ticket,"state":positive}),
                serde_json::json!({"ticket":ticket,"state":negative})
            ]
        );
        assert!(fixture.script.io_fault().is_none());
        for snapshots in [true, false] {
            let guard = ScriptIo::start(&fixture.directory.join("ui.json")).unwrap();
            if snapshots {
                assert!(!guard.snapshot(
                    &fixture.directory.join("ui.user-pause.extra.json"),
                    b"{}".to_vec()
                ));
            } else {
                assert!(
                    guard
                        .append(
                            &fixture.directory.join("ui.user-pause.extra.acks.jsonl"),
                            "{}".into()
                        )
                        .is_none()
                );
            }
            assert!(guard.status().fault.is_some());
        }
    }

    #[test]
    fn screen_state_roundtrips_only_through_its_fixed_bounded_path() {
        let fixture = Fixture::new(serde_json::json!([]));
        let snapshot = serde_json::json!({"user":0,"busy":false,
            "snapshot":{"analysis":{"type":"pixel","rgba":[11,37,73,255]}},
            "template_size":[8,8]});
        fixture.script.screen_state(&snapshot);
        fixture.script.wait_for_io();
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.directory.join("ui.state.screen.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved, snapshot);
        assert!(fixture.script.io_fault().is_none());
        let guard = ScriptIo::start(&fixture.directory.join("ui.json")).unwrap();
        assert!(!guard.snapshot(
            &fixture.directory.join("ui.state.screen.extra.json"),
            b"{}".to_vec()
        ));
        assert!(guard.status().fault.is_some());
    }
    #[test]
    fn mapper_state_and_both_ack_outcomes_roundtrip_through_scoped_real_io() {
        let fixture = Fixture::new(serde_json::json!([]));
        let snapshot = crate::mapper_ui::MapperPage::default().snapshot();
        let positive = crate::mapper_runtime::State {
            acknowledged: true,
            reply_to: Some(41),
            revision: 2,
            sequence: 9,
            ..Default::default()
        };
        let negative = crate::mapper_runtime::State {
            acknowledged: false,
            reply_to: Some(42),
            sequence: 10,
            error: Some("Stale mapper revision".into()),
            ..positive.clone()
        };
        fixture.script.mapper_state(&snapshot);
        fixture.script.mapper_ack(&positive);
        fixture.script.mapper_ack(&negative);
        fixture
            .script
            .mapper_ack(&crate::mapper_runtime::State::default());
        fixture.script.wait_for_io();
        let published: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.directory.join("ui.mapper.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(published, serde_json::to_value(&snapshot).unwrap());
        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(fixture.directory.join("ui.mapper.acks.jsonl"))
                .unwrap()
                .lines()
                .map(|row| serde_json::from_str(row).unwrap())
                .collect();
        assert_eq!(
            rows,
            vec![
                serde_json::to_value(&positive).unwrap(),
                serde_json::to_value(&negative).unwrap()
            ]
        );
        assert!(fixture.script.io_fault().is_none());

        for snapshot_path in [true, false] {
            let guard = ScriptIo::start(&fixture.directory.join("ui.json")).unwrap();
            if snapshot_path {
                assert!(!guard.snapshot(
                    &fixture.directory.join("ui.mapper.extra.json"),
                    b"{}".to_vec()
                ));
            } else {
                assert!(
                    guard
                        .append(
                            &fixture.directory.join("ui.mapper.extra.acks.jsonl"),
                            "{}".into()
                        )
                        .is_none()
                );
            }
            assert!(guard.status().fault.is_some());
            drop(guard);
        }
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn whole_response_scrolls_instead_of_staging_a_partly_clipped_button() {
        fn draw(ctx: &egui::Context, script: &mut Script, clipped: bool) -> egui::Response {
            let mut result = None;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                if clipped {
                    let mut clip = ui.clip_rect();
                    clip.max.y = ui.cursor().min.y + 15.0;
                    ui.set_clip_rect(clip);
                }
                let response = ouroboros_ui::atoms::Button::new("Details").sm().show(ui);
                script.observe("Details", &response);
                result = Some(response);
            });
            result.unwrap()
        }

        let mut fixture = Fixture::new(serde_json::json!([{
            "button":"Details", "wait_ms":0, "whole_response":true
        }]));
        let ctx = draw_test_context();
        for _ in 0..3 {
            let response = draw(&ctx, &mut fixture.script, true);
            assert!(response.interact_rect.contains(response.rect.center()));
            assert!(!response.interact_rect.contains_rect(response.rect));
            assert_eq!(fixture.script.response_visible, Some(false));
            assert!(fixture.script.hit.is_none());
            assert_eq!(fixture.script.stable_hits, 0);
            fixture.settle_events();
        }
        for _ in 0..3 {
            let response = draw(&ctx, &mut fixture.script, false);
            assert!(response.interact_rect.contains_rect(response.rect));
            fixture.settle_events();
        }
        assert_eq!(fixture.script.response_visible, Some(true));
        assert!(fixture.script.hit.is_some());
        assert!(fixture.script.stable_hits > 0);
        let point = fixture.script.hit.unwrap();
        fixture.script.awaiting_click = true;
        for pressed in [true, false] {
            let input = egui::RawInput {
                focused: true,
                events: vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let response = ouroboros_ui::atoms::Button::new("Details").sm().show(ui);
                assert_eq!(response.clicked(), !pressed);
                assert_eq!(fixture.script.observe("Details", &response), !pressed);
            });
            fixture.settle_events();
        }
        assert!(fixture.script.last_event_sequence.is_some());
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();

        // Existing scripts retain their center-visible staging behavior.
        let mut legacy = Fixture::new(serde_json::json!([{
            "button":"Details", "wait_ms":0
        }]));
        draw(&draw_test_context(), &mut legacy.script, true);
        assert_eq!(legacy.script.response_visible, Some(true));
        assert!(legacy.script.hit.is_some());
        let directory = legacy.directory.clone();
        drop(legacy);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn window_selection_script_rejects_candidate_field_injection() {
        let fixture = Fixture::new(serde_json::json!([]));
        let path = fixture.directory.join("ui.json");
        for field in [
            "selection_pid",
            "selection_hwnd",
            "selection_creation_token",
        ] {
            std::fs::write(
                &path,
                serde_json::json!([{
                    "button": "Select window", "wait_ms": 0, "fields": { (field): "42" }
                }])
                .to_string(),
            )
            .unwrap();
            let error = Script::load(path.clone())
                .err()
                .expect("candidate injection must fail");
            assert!(error.to_string().contains("actual Window chooser"));
        }
    }

    #[test]
    fn mapper_fields_stage_controls_but_reject_graph_user_and_native_handle_injection() {
        let fixture = Fixture::new(serde_json::json!([{
            "button": "Update mapper node", "wait_ms": 0,
            "fields": { "mapper_catalogue": "scale_delta", "mapper_parameter.factor": "2.5" }
        }]));
        assert_eq!(
            fixture.script.field_edits().unwrap()["mapper_parameter.factor"],
            "2.5"
        );
        let flow = Fixture::new(serde_json::json!([
            {"button": "Save mapper flow", "wait_ms": 0, "fields": {"mapper_flow_name": "Trail"}},
            {"button": "Import mapper flow", "wait_ms": 0, "fields": {"mapper_flow_path": "trail.json"}}
        ]));
        assert_eq!(
            flow.script.field_edits().unwrap()["mapper_flow_name"],
            "Trail"
        );
        let path = fixture.directory.join("ui.json");
        for field in [
            "mapper_flow_name",
            "mapper_flow_path",
            "mapper_flow_selected",
            "mapper_group_label",
            "mapper_flow_search",
            "mapper_node_search",
            "mapper_node_category",
            "mapper_definition",
            "mapper_nodes",
            "mapper_parameter.user",
            "mapper_parameter.type",
            "mapper_parameter.hwnd",
            "mapper_parameter.channel.handle",
        ] {
            std::fs::write(
                &path,
                serde_json::json!([{
                    "button": "Update mapper node", "wait_ms": 0, "fields": { (field): "42" }
                }])
                .to_string(),
            )
            .unwrap();
            let error = Script::load(path.clone())
                .err()
                .expect("mapper field injection must fail");
            assert!(error.to_string().contains("Input Mapper script fields"));
        }
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mapper_text_literals_are_bounded_and_confined_to_update_node() {
        let fixture = Fixture::new(serde_json::json!([{
            "button":"Update mapper node", "wait_ms":0,
            "fields":{"mapper_parameter.needle":"Ready é", "mapper_parameter.expected":""}
        }]));
        assert_eq!(
            fixture.script.field_edits().unwrap()["mapper_parameter.needle"],
            "Ready é"
        );
        let path = fixture.directory.join("ui.json");
        for (button, value) in [
            ("Apply mapper", "Ready".to_owned()),
            ("Update mapper node", "é".repeat(129)),
            ("Update mapper node", "forbidden\0control".to_owned()),
        ] {
            std::fs::write(
                &path,
                serde_json::json!([{
                    "button":button,"wait_ms":0,"fields":{"mapper_parameter.needle":value}
                }])
                .to_string(),
            )
            .unwrap();
            assert!(Script::load(path.clone()).is_err());
        }
    }
    #[test]
    fn mapper_palette_fields_are_bounded_and_only_stage_the_visible_search_action() {
        let fixture = Fixture::new(serde_json::json!([{
            "button": "Search mapper nodes", "wait_ms": 0,
            "fields": { "mapper_node_search": "  NORMALIZE  ", "mapper_node_category": "vector" }
        }]));
        let fields = fixture.script.field_edits().unwrap();
        assert_eq!(fields["mapper_node_search"], "  NORMALIZE  ");
        assert_eq!(fields["mapper_node_category"], "vector");
        let path = fixture.directory.join("ui.json");
        for (button, field, value) in [
            ("Search mapper nodes", "mapper_node_search", "é".repeat(65)),
            (
                "Search mapper nodes",
                "mapper_node_search",
                "line\nnext".into(),
            ),
            (
                "Search mapper nodes",
                "mapper_node_search",
                "hidden\u{0085}control".into(),
            ),
            (
                "Search mapper nodes",
                "mapper_node_category",
                "hardware".into(),
            ),
            ("Add mapper node", "mapper_node_search", "pointer".into()),
            ("Arm input mapper", "mapper_node_category", "sources".into()),
        ] {
            std::fs::write(
                &path,
                serde_json::json!([{
                    "button": button, "wait_ms": 0, "fields": { (field): value }
                }])
                .to_string(),
            )
            .unwrap();
            assert!(Script::load(path.clone()).is_err(), "{button}: {field}");
        }
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bootstrap_disabled_retry_can_sample_while_public_ready_is_false() {
        let mut fixture = Fixture::new(serde_json::json!([
            {"button":"Retry","wait_ms":0,"expect_disabled":true,"wait_for_sample":true},
            {"button":"Retry","wait_ms":0,"expect_disabled":true}
        ]));
        let context = draw_test_context();
        fixture.script.ready(false);
        fixture.script.bootstrap_state("initializing", None);
        assert!(!fixture.script.was_ready);
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let response = ouroboros_ui::atoms::Button::new("Retry")
                .enabled(false)
                .show(ui);
            assert!(!fixture.script.observe("Retry", &response));
        });
        fixture.settle_events();
        fixture.script.input(&mut egui::RawInput::default());
        assert_eq!(fixture.script.completed, 1);
        assert!(fixture.script.awaiting_sample);
        std::fs::write(&fixture.script.sampled_path, "1").unwrap();
        fixture
            .script
            .script_io
            .wait_for(|status| status.sampled == Some(1));
        fixture.script.input(&mut egui::RawInput::default());
        assert!(!fixture.script.awaiting_sample);

        // Dashboard readiness calls ready() without bootstrap_state().
        fixture.script.ready(false);
        assert!(!fixture.script.bootstrap_shell);
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let response = ouroboros_ui::atoms::Button::new("Retry")
                .enabled(false)
                .show(ui);
            assert!(!fixture.script.observe("Retry", &response));
        });
        fixture.settle_events();
        fixture.script.input(&mut egui::RawInput::default());
        assert_eq!(fixture.script.completed, 1);
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn held_sample_blocks_next_step_observation_and_draft_edits() {
        let mut fixture = Fixture::new(serde_json::json!([
            {"button":"Previous","wait_ms":0,"wait_for_sample":true},
            {
                "button":"Next","wait_ms":0,"point":[7,8],"fields":{
                    "region_left":"42",
                    "annotation_export_kind":"desktop_region",
                    "annotation_export_region_left":"-1280",
                    "annotation_export_region_top":"0",
                    "annotation_export_region_right":"0",
                    "annotation_export_region_bottom":"720",
                    "agent_desktop_capture":"true"
                }
            }
        ]));
        fixture.script.complete_step(true);
        assert_eq!(fixture.script.steps.len(), 2);
        assert_eq!(fixture.script.completed, 0);
        fixture.settle_events();
        fixture.script.input(&mut egui::RawInput::default());
        assert!(fixture.script.awaiting_sample);
        assert!(fixture.script.field_edits().is_none());
        assert!(fixture.script.position_edit().is_none());
        let context = egui::Context::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let response = ui.button("Next");
            assert!(!fixture.script.observe("Next", &response));
        });
        assert!(fixture.script.response_label.is_none());
        assert!(fixture.script.latest_response.is_none());

        std::fs::write(&fixture.script.sampled_path, "1").unwrap();
        // Polling is already armed. Observe publication before input consumes
        // and clears the receipt; otherwise this wait races that consumption.
        fixture
            .script
            .script_io
            .wait_for(|status| status.sampled == Some(1));
        fixture.script.input(&mut egui::RawInput::default());
        assert!(!fixture.script.awaiting_sample);
        fixture.settle_events();
        assert_eq!(
            fixture.script.field_edits().unwrap().get("region_left"),
            Some(&"42".to_owned())
        );
        let fields = fixture.script.field_edits().unwrap();
        assert_eq!(
            fields.get("annotation_export_kind").unwrap(),
            "desktop_region"
        );
        assert_eq!(
            fields.get("annotation_export_region_left").unwrap(),
            "-1280"
        );
        assert_eq!(
            fields.get("annotation_export_region_bottom").unwrap(),
            "720"
        );
        assert_eq!(fields.get("agent_desktop_capture").unwrap(), "true");
        assert_eq!(fixture.script.position_edit(), Some((0, [7, 8])));
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn in_flight_pointer_release_survives_blocked_phase_and_fault_cancels_press() {
        fn input(point: Pos2, pressed: bool) -> egui::RawInput {
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                events: vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            }
        }
        fn center(ctx: &egui::Context, script: &mut Script) -> Pos2 {
            let mut point = Pos2::ZERO;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let response = ui.button("Action");
                point = response.rect.center();
                script.observe("Action", &response);
            });
            point
        }

        let mut fixture = Fixture::new(serde_json::json!([{"button":"Action","wait_ms":100000}]));
        let path = fixture.directory.join("ui.json");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        fixture.script.script_io =
            ScriptIo::start_with_phase_barrier(&path, started_tx, release_rx).unwrap();
        let ctx = egui::Context::default();
        let point = center(&ctx, &mut fixture.script);
        let _ = ctx.run_ui(input(point, true), |ui| {
            let response = ui.button("Action");
            assert!(!response.clicked());
        });
        fixture.script.awaiting_click = true;
        fixture.script.awaiting_post_release_observe = true;
        fixture.script.phase_event("injected_pointer_release");
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let probe = crate::gui_activity::Probe::new();
        let (mut app_handle, requests) = crate::gui_bridge::app_channel();
        app_handle.attach_activity(probe.observer());
        let before = app_handle.activity_snapshot().unwrap();
        let release_input = input(point, false);
        {
            let _activity = probe.enter(crate::gui_activity::Callback::RawInput);
            fixture.script.capture(&ctx, &release_input);
        }
        {
            let _activity = probe.enter(crate::gui_activity::Callback::Logic);
            fixture.script.ready(false);
        }
        let mut accepted = false;
        let ui_activity = probe.enter(crate::gui_activity::Callback::Ui);
        let _ = ctx.run_ui(input(point, false), |ui| {
            let response = ui.button("Action");
            assert!(
                response.clicked(),
                "egui must produce the one release click"
            );
            accepted = fixture.script.observe("Action", &response);
        });
        drop(ui_activity);
        assert!(accepted, "the in-flight click cannot wait for phase I/O");
        let (activity_tx, activity_rx) = mpsc::channel();
        let reader = app_handle.clone();
        let thread = std::thread::spawn(move || {
            activity_tx.send(reader.activity_snapshot()).unwrap();
        });
        let observed = activity_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("activity read waited for GUI or script publication")
            .unwrap();
        thread.join().unwrap();
        assert!(observed.coherent);
        assert_eq!(
            (observed.process_id, observed.gui_instance),
            (before.process_id, before.gui_instance)
        );
        let observation = observed.observation.unwrap();
        assert_eq!(
            (
                observation.raw_input.returned,
                observation.logic.returned,
                observation.ui.returned
            ),
            (1, 1, 1)
        );
        assert!(!observation.in_callback && !observation.closed);
        assert!(
            requests.try_recv().is_err(),
            "metadata must not enqueue a GUI request"
        );
        assert!(
            !fixture.directory.join("ui.progress.json").exists(),
            "progress publication must remain behind the blocked phase"
        );
        assert_eq!(fixture.script.steps.len(), 1);
        let event_sequence = fixture.script.last_event_sequence.unwrap();
        assert!(!fixture.script.script_io.allows(Some(event_sequence)));
        fixture.script.input(&mut egui::RawInput::default());
        assert_eq!(fixture.script.completed, 0);
        release_tx.send(()).unwrap();
        fixture.settle_events();
        fixture.script.input(&mut egui::RawInput::default());
        assert_eq!(fixture.script.completed, 1);
        fixture.script.wait_for_io();
        let events = std::fs::read_to_string(fixture.directory.join("ui.events.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&events).unwrap()["clicked"],
            true
        );
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();

        let directory = std::env::temp_dir().join(format!(
            "protoinputmultiplex-cancel-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("ui.json");
        std::fs::write(&path, r#"[{"button":"Action","wait_ms":100000}]"#).unwrap();
        let mut script = Script::load(path).unwrap();
        let ctx = egui::Context::default();
        let point = center(&ctx, &mut script);
        let _ = ctx.run_ui(input(point, true), |ui| {
            let response = ui.button("Action");
            assert!(!response.clicked());
        });
        script.release = Some(point);
        assert!(script.script_io.line_json(&"x".repeat(65_536)).is_none());
        let mut cancelled = egui::RawInput::default();
        script.input(&mut cancelled);
        assert_eq!(cancelled.events.len(), 3);
        assert!(matches!(
            cancelled.events.last(),
            Some(egui::Event::PointerGone)
        ));
        let _ = ctx.run_ui(cancelled, |ui| {
            let response = ui.button("Action");
            assert!(
                !response.clicked(),
                "fault cancellation must not activate the control"
            );
        });
        let mut repeated = egui::RawInput::default();
        script.input(&mut repeated);
        assert!(repeated.events.is_empty(), "fault cancellation emits once");
        assert_eq!(script.completed, 0);
        drop(script);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_script_traverses_focus_adjusts_value_and_activates_real_buttons() {
        let mut fixture = Fixture::new(serde_json::json!([
            {"button":"First","key":"Tab","wait_ms":0},
            {"button":"First","key":"Enter","wait_ms":0},
            {"button":"Value","key":"Tab","wait_ms":0},
            {"button":"Value","key":"ArrowRight","wait_ms":0},
            {"button":"Apply","key":"Tab","wait_ms":0},
            {"button":"Apply","key":"Enter","wait_ms":0}
        ]));
        let ctx = egui::Context::default();
        let mut value = 1.0;
        let mut activations = [0, 0];
        for frame in 0..120 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                time: Some(f64::from(frame) / 60.0),
                ..Default::default()
            };
            fixture.script.input(&mut input);
            let _ = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let first = ui.button("First");
                    let accepted = fixture.script.observe("First", &first);
                    if first.clicked() && accepted {
                        activations[0] += 1;
                    }
                    let slider = crate::keyboard_slider::accessible_slider(
                        ui, "value", "Value", &mut value, 0.1, 8.0, 0.1,
                    );
                    fixture.script.observe("Value", &slider);
                    let apply = ui.button("Apply");
                    let accepted = fixture.script.observe("Apply", &apply);
                    if apply.clicked() && accepted {
                        activations[1] += 1;
                    }
                });
            });
            fixture.settle_events();
            if fixture.script.steps.is_empty() {
                break;
            }
        }
        assert!(fixture.script.steps.is_empty());
        assert_eq!(activations, [1, 1]);
        assert!((value - 1.1).abs() < 0.0001);
        let events: Vec<serde_json::Value> =
            std::fs::read_to_string(fixture.directory.join("ui.events.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert_eq!(events.len(), 6);
        assert!(events.iter().all(|event| event["focused"] == true));
        assert_eq!(events[3]["changed"], true);
        assert!(
            events
                .iter()
                .all(|event| event["key_pairs"].as_u64().unwrap() <= 24)
        );
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn annotation_seats() -> Vec<mux_core::Seat> {
        serde_json::from_value(serde_json::json!([
            {"id":"a","name":"A","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":0},
            {"id":"b","name":"B","mouse":"virtual://mouse/2","keyboard":"virtual://keyboard/2","target_pid":0}
        ]))
        .unwrap()
    }

    type DrawKeyEvent = (egui::Key, bool, bool, egui::Modifiers);
    type DrawStepResult = (
        Option<crate::annotation_ui::PageAction>,
        Vec<DrawKeyEvent>,
        Vec<serde_json::Value>,
    );

    fn drive_draw_step(
        fixture: &mut Fixture,
        page: &mut crate::annotation_ui::AnnotationPage,
        seats: &[mux_core::Seat],
    ) -> DrawStepResult {
        let ctx = draw_test_context();
        let mut action_frame = 0;
        let mut frame_actions = BTreeSet::new();
        let mut dispatched = None;
        let mut key_events = Vec::new();
        for frame in 0..80 {
            let mut input = egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(900.0, 1200.0),
                )),
                time: Some(f64::from(frame) / 60.0),
                ..Default::default()
            };
            fixture.script.input(&mut input);
            for event in &input.events {
                match event {
                    egui::Event::Key {
                        key,
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } => {
                        key_events.push((*key, *pressed, *repeat, *modifiers));
                    }
                    _ => panic!("Draw access-key script must use only egui key events"),
                }
            }
            let _ = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let action = page.show(
                        ui,
                        seats,
                        false,
                        true,
                        true,
                        true,
                        false,
                        false,
                        Some(&mut fixture.script),
                        &mut action_frame,
                        &mut frame_actions,
                    );
                    if action.is_some() {
                        assert!(dispatched.is_none(), "a single access key dispatched twice");
                        dispatched = action;
                    }
                });
            });
            fixture.settle_events();
            if fixture.script.steps.is_empty() {
                break;
            }
        }
        assert!(
            fixture.script.steps.is_empty(),
            "Draw access step never observed its effect"
        );
        let events = std::fs::read_to_string(fixture.directory.join("ui.events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (dispatched, key_events, events)
    }

    fn assert_exact_alt_pair(events: &[(egui::Key, bool, bool, egui::Modifiers)], key: egui::Key) {
        assert_eq!(
            events,
            &[
                (key, true, false, egui::Modifiers::ALT),
                (key, false, false, egui::Modifiers::ALT),
            ]
        );
    }

    #[test]
    fn local_alt_clear_and_save_dispatch_the_draw_pages_existing_commands() {
        let seats = annotation_seats();
        let mut clear = Fixture::new(serde_json::json!([
            {"button":"Clear selected user's ink","key":"AltC","wait_ms":0}
        ]));
        let mut page = crate::annotation_ui::AnnotationPage::default();
        assert!(page.focus_user(&seats, 1));
        let (action, key_events, observations) = drive_draw_step(&mut clear, &mut page, &seats);
        assert_exact_alt_pair(&key_events, egui::Key::C);
        assert!(matches!(
            action,
            Some(crate::annotation_ui::PageAction::Worker(
                crate::annotation_runtime::Command::ClearUser { user: 1 }
            ))
        ));
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["shortcut_activated"], true);
        let directory = clear.directory.clone();
        drop(clear);
        std::fs::remove_dir_all(directory).unwrap();

        let mut all_seats = annotation_seats();
        all_seats[1].annotation.clear_all = true;
        let mut clear_all = Fixture::new(serde_json::json!([
            {"button":"Clear all users' ink","key":"AltC","wait_ms":0}
        ]));
        let mut page = crate::annotation_ui::AnnotationPage::default();
        assert!(page.focus_user(&all_seats, 1));
        let (action, key_events, observations) =
            drive_draw_step(&mut clear_all, &mut page, &all_seats);
        assert_exact_alt_pair(&key_events, egui::Key::C);
        assert!(matches!(
            action,
            Some(crate::annotation_ui::PageAction::Worker(
                crate::annotation_runtime::Command::ClearAll
            ))
        ));
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["shortcut_activated"], true);
        let directory = clear_all.directory.clone();
        drop(clear_all);
        std::fs::remove_dir_all(directory).unwrap();

        let mut save = Fixture::new(serde_json::json!([
            {"button":"Save annotation PNG","key":"AltV","wait_ms":0}
        ]));
        let mut page = crate::annotation_ui::AnnotationPage::default();
        assert!(page.focus_user(&seats, 1));
        page.apply_script_fields(&BTreeMap::from([(
            "annotation_export_path".to_owned(),
            "C:\\Temp\\draw.png".to_owned(),
        )]));
        let (action, key_events, observations) = drive_draw_step(&mut save, &mut page, &seats);
        assert_exact_alt_pair(&key_events, egui::Key::V);
        assert!(matches!(
            action,
            Some(crate::annotation_ui::PageAction::Worker(
                crate::annotation_runtime::Command::Export {
                    user: 1,
                    kind: crate::annotation_runtime::ExportKind::InkOnly,
                    ink_scope: crate::annotation_runtime::InkScope::AllUsers,
                    desktop_region: None,
                    ..
                }
            ))
        ));
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["shortcut_activated"], true);
        let directory = save.directory.clone();
        drop(save);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_alt_draw_and_eraser_complete_only_after_staging_the_tool() {
        let seats = annotation_seats();
        for (label, key_name, key) in [
            ("Annotation Brush", "AltD", egui::Key::D),
            ("Annotation Eraser", "AltE", egui::Key::E),
        ] {
            let mut fixture = Fixture::new(serde_json::json!([
                {"button":label,"key":key_name,"wait_ms":0}
            ]));
            let mut page = crate::annotation_ui::AnnotationPage::default();
            let (action, key_events, observations) =
                drive_draw_step(&mut fixture, &mut page, &seats);
            assert!(
                action.is_none(),
                "tool change must remain a draft until Apply"
            );
            assert_exact_alt_pair(&key_events, key);
            assert_eq!(observations.len(), 1);
            assert_eq!(observations[0]["shortcut_activated"], true);
            let directory = fixture.directory.clone();
            drop(fixture);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn local_alt_focus_keys_complete_after_the_draw_control_has_focus() {
        let seats = annotation_seats();
        for (label, key_name, egui_key) in [
            ("Clear shortcut", "AltS", egui::Key::S),
            ("Brush color", "AltO", egui::Key::O),
            ("Activator Primary", "AltB", egui::Key::B),
        ] {
            let mut fixture = Fixture::new(serde_json::json!([
                {"button":label,"key":key_name,"wait_ms":0}
            ]));
            let mut page = crate::annotation_ui::AnnotationPage::default();
            let (action, key_events, observations) =
                drive_draw_step(&mut fixture, &mut page, &seats);
            assert!(action.is_none());
            assert_exact_alt_pair(&key_events, egui_key);
            assert_eq!(observations.len(), 1);
            assert_eq!(observations[0]["button"], label);
            assert_eq!(observations[0]["focused"], true);
            if key_name == "AltO" {
                assert_eq!(observations[0]["color_editor_open"], true);
            }
            let directory = fixture.directory.clone();
            drop(fixture);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn local_magnifier_alt_keys_complete_only_after_draft_toggle_and_apply_dispatch() {
        let seats = annotation_seats();
        let mut fixture = Fixture::new(serde_json::json!([
            {"button":"Show lens border","key":"AltB","wait_ms":0},
            {"button":"Refresh automatically","key":"AltU","wait_ms":0},
            {"button":"Apply magnifier settings","wait_ms":0}
        ]));
        let ctx = draw_test_context();
        let mut page = crate::magnifier_ui::MagnifierPage::default();
        let mut action_frame = 0;
        let mut frame_actions = BTreeSet::new();
        let mut key_events = Vec::new();
        let mut dispatched = None;
        for frame in 0..160 {
            let mut input = egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(840.0, 1000.0),
                )),
                time: Some(f64::from(frame) / 60.0),
                ..Default::default()
            };
            fixture.script.input(&mut input);
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } = event
                {
                    key_events.push((*key, *pressed, *repeat, *modifiers));
                }
            }
            let _ = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let action = page.show(
                        ui,
                        &seats,
                        false,
                        &[],
                        true,
                        true,
                        true,
                        true,
                        false,
                        None,
                        Some(&mut fixture.script),
                        &mut action_frame,
                        &mut frame_actions,
                    );
                    if action.is_some() {
                        assert!(dispatched.is_none(), "Apply dispatched more than once");
                        dispatched = action;
                    }
                });
            });
            fixture.settle_events();
            if fixture.script.steps.is_empty() && dispatched.is_some() {
                break;
            }
        }
        assert!(fixture.script.steps.is_empty());
        assert_eq!(key_events.len(), 4);
        assert_exact_alt_pair(&key_events[..2], egui::Key::B);
        assert_exact_alt_pair(&key_events[2..], egui::Key::U);
        let observations: Vec<serde_json::Value> =
            std::fs::read_to_string(fixture.directory.join("ui.events.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0]["shortcut_activated"], true);
        assert_eq!(observations[1]["shortcut_activated"], true);
        assert_eq!(observations[0]["key_pairs"], 1);
        assert_eq!(observations[1]["key_pairs"], 1);
        assert_eq!(observations[2]["clicked"], true);
        let settings = match dispatched {
            Some(crate::magnifier_ui::PageAction::Worker(
                crate::magnifier_runtime::Command::Configure { user: 0, settings },
            )) => settings,
            _ => panic!("Apply must use the selected user's existing Configure command"),
        };
        assert_eq!(settings.border, !seats[0].magnifier.border);
        assert_eq!(settings.auto_refresh, !seats[0].magnifier.auto_refresh);
        assert_eq!(page.selected_user(), 0);

        let users = seats
            .iter()
            .enumerate()
            .map(|(user, seat)| crate::magnifier_runtime::UserState {
                user,
                settings: if user == 0 {
                    settings.clone()
                } else {
                    seat.magnifier.clone()
                },
                status: "Ready".into(),
                source_bounds: None,
                source_hwnd: None,
                source_pid: None,
                cursor: None,
                frame_cursor: None,
                capture_sequence: 0,
                frame_age_ms: None,
                in_flight: false,
                overlay: None,
            })
            .collect();
        let accepted = page.receive(crate::magnifier_runtime::State {
            acknowledged: true,
            users,
            export: None,
            error: None,
        });
        assert!(accepted.is_some_and(|(user, applied)| user == 0 && applied == settings));
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_alt_action_does_not_complete_from_visibility_alone() {
        let mut fixture = Fixture::new(serde_json::json!([
            {"button":"Action","key":"AltD","wait_ms":0}
        ]));
        let ctx = draw_test_context();
        let mut sent = 0;
        for _ in 0..8 {
            let mut input = egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            };
            fixture.script.input(&mut input);
            sent += input.events.len();
            let _ = ctx.run_ui(input, |ui| {
                let response = ui.button("Action");
                fixture.script.observe("Action", &response);
            });
        }
        assert_eq!(sent, 2, "one local Alt key down/up pair should be sent");
        assert_eq!(fixture.script.steps.len(), 1);
        let directory = fixture.directory.clone();
        drop(fixture);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, serde::Serialize)]
enum ScriptKey {
    Tab,
    ShiftTab,
    Enter,
    Space,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    PageUp,
    PageDown,
    AltD,
    AltE,
    AltC,
    AltO,
    AltB,
    AltU,
    AltS,
    AltH,
    AltA,
    AltV,
}
impl ScriptKey {
    fn egui(self) -> egui::Key {
        match self {
            Self::Tab | Self::ShiftTab => egui::Key::Tab,
            Self::Enter => egui::Key::Enter,
            Self::Space => egui::Key::Space,
            Self::ArrowLeft => egui::Key::ArrowLeft,
            Self::ArrowRight => egui::Key::ArrowRight,
            Self::ArrowUp => egui::Key::ArrowUp,
            Self::ArrowDown => egui::Key::ArrowDown,
            Self::Home => egui::Key::Home,
            Self::End => egui::Key::End,
            Self::PageUp => egui::Key::PageUp,
            Self::PageDown => egui::Key::PageDown,
            Self::AltD => egui::Key::D,
            Self::AltE => egui::Key::E,
            Self::AltC => egui::Key::C,
            Self::AltO => egui::Key::O,
            Self::AltB => egui::Key::B,
            Self::AltU => egui::Key::U,
            Self::AltS => egui::Key::S,
            Self::AltH => egui::Key::H,
            Self::AltA => egui::Key::A,
            Self::AltV => egui::Key::V,
        }
    }
    fn access_key(self) -> bool {
        matches!(
            self,
            Self::AltD
                | Self::AltE
                | Self::AltC
                | Self::AltO
                | Self::AltB
                | Self::AltU
                | Self::AltS
                | Self::AltH
                | Self::AltA
                | Self::AltV
        )
    }
    fn focus_access_key_for(self, label: &str) -> bool {
        matches!((self, label), (Self::AltS, "Clear shortcut"))
            || (matches!(self, Self::AltB) && label.starts_with("Activator "))
    }
    fn traversal(self) -> bool {
        matches!(self, Self::Tab | Self::ShiftTab)
    }
    fn activation(self) -> bool {
        matches!(self, Self::Enter | Self::Space)
    }
}
pub struct Script {
    steps: VecDeque<Step>,
    started: Instant,
    hit: Option<Pos2>,
    last_hit: Option<Pos2>,
    stable_hits: u8,
    release: Option<Pos2>,
    awaiting_click: bool,
    was_ready: bool,
    bootstrap_shell: bool,
    script_io: ScriptIo,
    events_path: PathBuf,
    pending_event_sequence: Option<u64>,
    last_event_sequence: Option<u64>,
    pending_phase_sequence: Option<u64>,
    state_path: PathBuf,
    discovery_path: PathBuf,
    launch_path: PathBuf,
    window_lock_path: PathBuf,
    window_selection_path: PathBuf,
    recovery_path: PathBuf,
    annotation_path: PathBuf,
    magnifier_path: PathBuf,
    macro_studio_path: PathBuf,
    macro_file_path: PathBuf,
    virtual_users_path: PathBuf,
    agent_path: PathBuf,
    graph_path: PathBuf,
    mapper_path: PathBuf,
    user_pause_path: PathBuf,
    output_path: PathBuf,
    app_preferences_path: PathBuf,
    profile_path: PathBuf,
    ready_path: PathBuf,
    progress_path: PathBuf,
    #[cfg(test)]
    sampled_path: PathBuf,
    completed: usize,
    awaiting_sample: bool,
    capture_worker: Option<CaptureWorker>,
    capture_activity: Option<crate::gui_activity::CaptureReporter>,
    capture_script_id: u64,
    capture_generation: u64,
    capture_received_generation: u64,
    last_capture: Instant,
    response_enabled: Option<bool>,
    response_visible: Option<bool>,
    response_label: Option<String>,
    keyboard_pairs: u8,
    keyboard_cooldown: u8,
    keyboard_target_focused: bool,
    phase_log_path: PathBuf,
    phase_keys: BTreeSet<(usize, String)>,
    diagnostic_started: Instant,
    diagnostic_frame: u64,
    diagnostic_hit: Option<Pos2>,
    awaiting_post_release_observe: bool,
    latest_raw_viewport: serde_json::Value,
    latest_response: Option<serde_json::Value>,
}
pub struct PreferencesUiStatus<'a> {
    pub operation: crate::gui_app_preferences::Status,
    pub quit_waiting: bool,
    pub notice: Option<&'a str>,
    pub error: Option<&'a str>,
    pub editor: serde_json::Value,
}
pub struct ProfileUiStatus {
    pub cursor_settings_sync: crate::cursor_settings_ui::Snapshot,
    pub profile_file: crate::gui_profile_file::Snapshot,
    pub exit_save: crate::gui_profile_policy::Status,
}

impl Script {
    #[cfg(test)]
    pub fn load(path: PathBuf) -> Result<Self> {
        Self::load_with_guards(path, None, true)
    }

    pub fn load_guarded(
        path: PathBuf,
        guards: std::sync::Arc<crate::gui::RuntimeGuards>,
        screenshots: bool,
    ) -> Result<Self> {
        Self::load_with_guards(path, Some(guards), screenshots)
    }

    fn load_with_guards(
        path: PathBuf,
        guards: Option<std::sync::Arc<crate::gui::RuntimeGuards>>,
        screenshots: bool,
    ) -> Result<Self> {
        static NEXT_CAPTURE_SCRIPT_ID: AtomicU64 = AtomicU64::new(1);
        let steps: VecDeque<Step> = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            steps
                .iter()
                .all(|step| step.key.is_none() || !step.expect_disabled),
            "A keyboard step cannot inject input into a disabled observation"
        );
        anyhow::ensure!(
            steps.iter().all(|step| !step.fields.keys().any(|field| {
                matches!(
                    field.as_str(),
                    "selection_pid" | "selection_hwnd" | "selection_creation_token"
                )
            })),
            "Window selection requires actual Window chooser and option clicks; candidate field injection is unsupported"
        );
        anyhow::ensure!(
            steps
                .iter()
                .all(|step| step.fields.iter().all(|(field, value)| {
                    if field == "mapper_catalogue" {
                        mux_core::mapper::catalogue()
                            .iter()
                            .any(|node| node.name == *value)
                    } else if field == "mapper_node_search" {
                        step.button == "Search mapper nodes"
                            && mux_core::mapper::browse_catalogue(value, None).is_ok()
                    } else if field == "mapper_node_category" {
                        step.button == "Search mapper nodes"
                            && (value == "all"
                                || serde_json::from_value::<mux_core::mapper::NodeCategory>(
                                    serde_json::Value::String(value.clone()),
                                )
                                .is_ok())
                    } else if field == "mapper_flow_search" {
                        step.button == "Search mapper flows"
                            && value.len() <= 128
                            && !value.chars().any(char::is_control)
                    } else if field == "mapper_group_label" {
                        step.button == "Rename mapper group"
                            && value.len() <= 128
                            && !value.chars().any(char::is_control)
                    } else if field == "mapper_flow_name" {
                        step.button == "Save mapper flow"
                            && value.len() <= 64
                            && !value.chars().any(char::is_control)
                    } else if field == "mapper_flow_path" {
                        matches!(
                            step.button.as_str(),
                            "Import mapper flow" | "Export mapper flow"
                        ) && value.len() <= 4096
                            && !value.contains('\0')
                    } else if matches!(
                        field.as_str(),
                        "mapper_parameter.needle" | "mapper_parameter.expected"
                    ) {
                        step.button == "Update mapper node"
                            && value.len() <= mux_core::mapper::MAX_TEXT_LITERAL_BYTES
                            && !value
                                .chars()
                                .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
                    } else if let Some(parameter) = field.strip_prefix("mapper_parameter.") {
                        matches!(
                            parameter,
                            "key"
                                | "x"
                                | "y"
                                | "min_score"
                                | "region.x"
                                | "region.y"
                                | "region.width"
                                | "region.height"
                                | "button"
                                | "factor"
                                | "radians"
                                | "radius"
                                | "min"
                                | "max"
                                | "initial"
                                | "offset.0"
                                | "offset.1"
                                | "min.0"
                                | "min.1"
                                | "max.0"
                                | "max.1"
                                | "initial.0"
                                | "initial.1"
                                | "value.value"
                                | "value.value.0"
                                | "value.value.1"
                                | "channel.key"
                                | "channel.button"
                        ) && value.len() <= 128
                    } else {
                        !field.starts_with("mapper_")
                    }
                })),
            "Input Mapper script fields only stage a catalogue choice or filter, supported numeric parameter or flow form value; graph, user and native-handle injection is unsupported"
        );
        let phase_log_path = path.with_extension("phases.jsonl");
        let _ = std::fs::File::create(&phase_log_path)?;
        let events_path = path.with_extension("events.jsonl");
        let _ = std::fs::File::create(&events_path)?;
        let script_io = if let Some(guards) = &guards {
            ScriptIo::start_guarded(&path, std::sync::Arc::clone(guards))?
        } else {
            ScriptIo::start(&path)?
        };
        let capture_worker = if !screenshots {
            None
        } else if let Some(guards) = guards {
            Some(CaptureWorker::start_guarded(
                path.with_extension("png"),
                guards,
            )?)
        } else {
            Some(CaptureWorker::start(path.with_extension("png"))?)
        };
        Ok(Self {
            script_io,
            events_path,
            pending_event_sequence: None,
            last_event_sequence: None,
            pending_phase_sequence: None,
            state_path: path.with_extension("state.json"),
            discovery_path: path.with_extension("discovery.json"),
            launch_path: path.with_extension("launch.json"),
            window_lock_path: path.with_extension("window-lock.json"),
            window_selection_path: path.with_extension("window-selection.json"),
            recovery_path: path.with_extension("recovery.json"),
            annotation_path: path.with_extension("annotation.json"),
            magnifier_path: path.with_extension("magnifier.json"),
            macro_studio_path: path.with_extension("macro-studio.json"),
            macro_file_path: path.with_extension("macro-file.json"),
            virtual_users_path: path.with_extension("virtual-users.json"),
            agent_path: path.with_extension("agent.json"),
            graph_path: path.with_extension("graph.json"),
            mapper_path: path.with_extension("mapper.json"),
            user_pause_path: path.with_extension("user-pause.json"),
            output_path: path.with_extension("output.json"),
            app_preferences_path: path.with_extension("app-preferences.json"),
            profile_path: path.with_extension("profile.json"),
            ready_path: path.with_extension("ready.json"),
            progress_path: path.with_extension("progress.json"),
            #[cfg(test)]
            sampled_path: path.with_extension("sampled.json"),
            completed: 0,
            awaiting_sample: false,
            capture_worker,
            capture_activity: None,
            capture_script_id: NEXT_CAPTURE_SCRIPT_ID.fetch_add(1, Ordering::Relaxed),
            capture_generation: 0,
            capture_received_generation: 0,
            last_capture: Instant::now(),
            response_enabled: None,
            response_visible: None,
            response_label: None,
            keyboard_pairs: 0,
            keyboard_cooldown: 0,
            keyboard_target_focused: false,
            phase_log_path,
            phase_keys: BTreeSet::new(),
            diagnostic_started: Instant::now(),
            diagnostic_frame: 0,
            diagnostic_hit: None,
            awaiting_post_release_observe: false,
            latest_raw_viewport: serde_json::json!({}),
            latest_response: None,
            steps,
            started: Instant::now(),
            hit: None,
            last_hit: None,
            stable_hits: 0,
            release: None,
            awaiting_click: false,
            was_ready: true,
            bootstrap_shell: false,
        })
    }
    fn write_snapshot(&self, path: impl AsRef<std::path::Path>, bytes: impl AsRef<[u8]>) -> bool {
        self.script_io
            .snapshot(path.as_ref(), bytes.as_ref().to_vec())
    }
    fn append_record(&self, path: impl AsRef<std::path::Path>, line: String) {
        let _ = self.script_io.append(path.as_ref(), line);
    }
    fn append_ack<T: serde::Serialize>(&self, enabled: bool, path: PathBuf, state: &T) {
        if enabled && let Some(json) = self.script_io.line_json(state) {
            self.append_record(path, json);
        }
    }
    fn io_gate(&self) -> bool {
        self.pending_event_sequence.is_none() && self.script_io.allows(self.pending_phase_sequence)
    }
    pub fn io_fault(&self) -> Option<String> {
        self.script_io.status().fault
    }
    #[cfg(test)]
    pub(crate) fn wait_for_io(&self) {
        self.script_io.wait_for(|status| {
            status.fault.is_some()
                || (status.acked_sequence == status.submitted_sequence
                    && status.pending_snapshots == 0
                    && !status.active)
        });
        assert!(self.io_fault().is_none());
    }
    pub fn ready(&mut self, ready: bool) {
        self.bootstrap_shell = false;
        let _ = self.write_snapshot(&self.ready_path, if ready { "true" } else { "false" });
        if ready && !self.was_ready {
            self.started = Instant::now();
        }
        self.was_ready = ready;
        let capture = self
            .capture_worker
            .as_ref()
            .map(CaptureWorker::status)
            .unwrap_or_default();
        let capture_render = serde_json::json!({
            "enabled": self.capture_worker.is_some(),
            "requested_generation": self.capture_generation,
            "received_generation": self.capture_received_generation,
        });
        let progress = serde_json::json!({"next":self.steps.front().map(|s| &s.button),"elapsed_ms":self.started.elapsed().as_millis(),"hit":self.hit.map(|p|[p.x,p.y]),"release":self.release.map(|p|[p.x,p.y]),"awaiting_click":self.awaiting_click,"ready":ready,"response_enabled":self.response_enabled,"response_visible":self.response_visible,"response_label":self.response_label,"key":self.steps.front().and_then(|s|s.key),"key_pairs":self.keyboard_pairs,"target_focused":self.keyboard_target_focused,"response":self.latest_response,"frame":self.diagnostic_frame,"viewport":self.latest_raw_viewport,"capture":capture,"capture_render":capture_render,"io":self.script_io.status(),"last_event_sequence":self.last_event_sequence});
        if let Some(bytes) = self.script_io.snapshot_json(&progress) {
            let _ = self.write_snapshot(&self.progress_path, bytes);
        }
    }
    pub fn position_edit(&self) -> Option<(usize, [i32; 2])> {
        if self.awaiting_sample || !self.io_gate() {
            return None;
        }
        self.steps
            .front()
            .and_then(|s| s.point.map(|p| (s.user, p)))
    }
    pub fn field_edits(&self) -> Option<&BTreeMap<String, String>> {
        if self.awaiting_sample || !self.io_gate() {
            return None;
        }
        self.steps
            .front()
            .map(|step| &step.fields)
            .filter(|fields| !fields.is_empty())
    }
    pub fn window_lock_state(&self, state: &serde_json::Value) {
        if let Some(bytes) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.window_lock_path, bytes);
        }
    }
    pub fn launch_state(
        &self,
        controller_present: bool,
        preflight_pending: bool,
        ready: bool,
        stopping: bool,
        setup: &serde_json::Value,
    ) {
        let value = serde_json::json!({
            "controller_present": controller_present,
            "preflight_pending": preflight_pending,
            "ready": ready,
            "stopping": stopping,
            "setup": setup,
        });
        if let Some(bytes) = self.script_io.snapshot_json(&value) {
            let _ = self.write_snapshot(&self.launch_path, bytes);
        }
    }
    pub fn bootstrap_state(&mut self, phase: &str, error: Option<&str>) {
        self.bootstrap_shell = true;
        let value = serde_json::json!({
            "controller_present": false,
            "preflight_pending": false,
            "ready": false,
            "stopping": false,
            "setup": {},
            "bootstrap": {"phase": phase, "error": error},
        });
        if let Some(bytes) = self.script_io.snapshot_json(&value) {
            let _ = self.write_snapshot(&self.launch_path, bytes);
        }
    }
    pub fn discovery_state(&self, status: &crate::gui_discovery::Status) {
        if let Some(json) = self.script_io.snapshot_json(status) {
            let _ = self.write_snapshot(&self.discovery_path, json);
        }
    }
    pub fn state(&self, states: &[crate::router::State]) {
        if let Some(json) = self.script_io.snapshot_json(states) {
            let _ = self.write_snapshot(&self.state_path, json);
        }
    }
    /// Opt-in inventory includes each actual chooser option's `label` for the
    /// next scripted click. Selection is driven only by observed Ouroboros
    /// responses through `observe`/`input`, never by candidate field edits.
    pub fn window_selection_state(&self, state: &serde_json::Value) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.window_selection_path, json);
        }
    }
    pub fn window_selection_ack(&self, state: &crate::window_selection::Acknowledgement) {
        self.append_ack(
            true,
            self.window_selection_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn recovery_state(&self, state: &crate::session_recovery::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.recovery_path, json);
        }
    }
    pub fn annotation_state(&self, state: &crate::annotation_runtime::State) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.annotation_path, json);
        }
        self.append_ack(
            state.acknowledged,
            self.annotation_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn screen_state(&self, state: &serde_json::Value) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(self.state_path.with_extension("screen.json"), json);
        }
    }
    pub fn magnifier_state(&self, state: &crate::magnifier_runtime::State) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.magnifier_path, json);
        }
        self.append_ack(
            state.acknowledged,
            self.magnifier_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn macro_studio_state(&self, state: &crate::macro_studio::State) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.macro_studio_path, json);
        }
        self.append_ack(
            state.acknowledged,
            self.macro_studio_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn macro_file_state(&self, state: &serde_json::Value) {
        if let Some(bytes) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.macro_file_path, bytes);
        }
    }
    pub fn virtual_users_state(&self, state: &crate::virtual_users_ui::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.virtual_users_path, json);
        }
    }
    pub fn virtual_users_ack(&self, state: &crate::virtual_users::State) {
        self.append_ack(
            state.acknowledged,
            self.virtual_users_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn graph_state(&self, state: &crate::replication_ui::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.graph_path, json);
        }
    }
    pub fn graph_ack(&self, state: &crate::automation_runtime::State) {
        self.append_ack(
            state.acknowledged,
            self.graph_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn forwarding_state(&self, state: &crate::forwarding_ui::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(
                self.graph_path.with_file_name("forwarding-state.json"),
                json,
            );
        }
    }
    pub fn forwarding_ack(&self, state: &crate::automation_runtime::State) {
        self.append_ack(
            state.acknowledged,
            self.graph_path.with_file_name("forwarding-acks.jsonl"),
            state,
        );
    }
    pub fn user_pause_state(&self, state: &serde_json::Value) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.user_pause_path, json);
        }
    }
    pub fn user_pause_ack(
        &self,
        ticket: &crate::session::SettingsTicket,
        state: &crate::routing_engine::UserPauseState,
    ) {
        self.append_ack(
            true,
            self.user_pause_path.with_extension("acks.jsonl"),
            &serde_json::json!({"ticket":ticket,"state":state}),
        );
    }
    pub fn mapper_state(&self, state: &crate::mapper_ui::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.mapper_path, json);
        }
    }
    pub fn mapper_ack(&self, state: &crate::mapper_runtime::State) {
        self.append_ack(
            state.reply_to.is_some() || state.acknowledged,
            self.mapper_path.with_extension("acks.jsonl"),
            state,
        );
    }
    /// Opt-in workflow evidence only; never part of the public agent permission surface.
    pub fn workflow_state(
        &self,
        output: &crate::output_ui::Snapshot,
        selected_user: Option<usize>,
        page: &str,
        category: &str,
    ) {
        if let Some(json) = self.script_io.snapshot_json(output) {
            let _ = self.write_snapshot(self.output_path.with_extension("page.json"), json);
        }
        let navigation = serde_json::json!({
            "selected_user": selected_user,
            "page": page,
            "category": category,
        });
        if let Some(json) = self.script_io.snapshot_json(&navigation) {
            let _ = self.write_snapshot(self.profile_path.with_extension("navigation.json"), json);
        }
    }

    pub fn output_state(&self, state: &crate::output_runtime::State) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.output_path, json);
        }
        self.append_ack(
            state.acknowledged,
            self.output_path.with_extension("acks.jsonl"),
            state,
        );
    }

    pub fn app_preferences_state(
        &self,
        committed: &crate::app_preferences::Preferences,
        draft: &crate::app_preferences::Preferences,
        preferences_path: &std::path::Path,
        ui_status: &PreferencesUiStatus<'_>,
    ) {
        let value = serde_json::json!({
            "committed": committed,
            "draft": draft,
            "path": preferences_path,
            "pending": ui_status.operation.pending,
            "error": ui_status.error.or(ui_status.operation.error.as_deref()),
            "operation": ui_status.operation,
            "quit_waiting": ui_status.quit_waiting,
            "notice": ui_status.notice,
            "editor": ui_status.editor,
        });
        if let Some(bytes) = self.script_io.snapshot_json(&value) {
            let _ = self.write_snapshot(&self.app_preferences_path, bytes);
        }
    }

    pub fn app_preferences_ack(&self, state: &crate::agent_runtime::AppPreferencesState) {
        self.append_ack(
            state.acknowledged,
            self.app_preferences_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn shortcut_status(&self, status: &crate::app_lifecycle::ShortcutStatus) {
        if let Some(bytes) = self.script_io.snapshot_json(status) {
            let _ = self.write_snapshot(
                self.app_preferences_path.with_extension("shortcuts.json"),
                bytes,
            );
        }
    }
    pub fn profile_state(
        &self,
        profile: &mux_core::Profile,
        undo_available: bool,
        active_path: &str,
        transfer_path: &str,
        status: &str,
        ui_status: &ProfileUiStatus,
    ) {
        let snapshot = serde_json::json!({
            "profile": profile,
            "undo_available": undo_available,
            "active_path": active_path,
            "transfer_path": transfer_path,
            "backup_path": crate::profile_store::backup_path(std::path::Path::new(active_path)),
            "status": status,
            "cursor_settings_sync": ui_status.cursor_settings_sync,
            "profile_file": ui_status.profile_file,
            "exit_save": ui_status.exit_save,
        });
        if let Some(json) = self.script_io.snapshot_json(&snapshot) {
            let _ = self.write_snapshot(&self.profile_path, json);
        }
    }
    pub fn agent_state(&self, state: &crate::agent_ui::Snapshot) {
        if let Some(json) = self.script_io.snapshot_json(state) {
            let _ = self.write_snapshot(&self.agent_path, json);
        }
    }
    pub fn agent_ack(&self, state: &crate::agent_ui::Snapshot, acknowledged: bool) {
        self.append_ack(
            acknowledged,
            self.agent_path.with_extension("acks.jsonl"),
            state,
        );
    }
    pub fn observe(&mut self, label: &str, response: &egui::Response) -> bool {
        // The completed step owns the viewport until its sample is acknowledged.
        // Even read-only observation of the next step can otherwise scroll it.
        let in_flight_release = self.awaiting_click
            && response.clicked()
            && self.steps.front().is_some_and(|step| {
                step.button == label && step.key.is_none() && !step.expect_disabled
            });
        if self.awaiting_sample
            || self.pending_event_sequence.is_some()
            || !self.script_io.allows(None)
            || (!in_flight_release && !self.script_io.allows(self.pending_phase_sequence))
        {
            return false;
        }
        if self.steps.front().is_some_and(|s| s.button == label) {
            // Ouroboros disables buttons by giving them hover-only interaction.
            let clickable = response.enabled() && response.sense.senses_click();
            self.response_enabled = Some(clickable);
            let whole_response = self.steps.front().is_some_and(|step| step.whole_response);
            self.response_visible = Some(if whole_response {
                response.interact_rect.contains_rect(response.rect)
            } else {
                response.interact_rect.contains(response.rect.center())
            });
            self.response_label = Some(label.to_owned());
            self.latest_response = Some(serde_json::json!({
                "clicked": response.clicked(),
                "enabled": clickable,
                "rect": [response.rect.min.x, response.rect.min.y,
                         response.rect.max.x, response.rect.max.y],
                "interact_rect": [response.interact_rect.min.x, response.interact_rect.min.y,
                                  response.interact_rect.max.x, response.interact_rect.max.y]
            }));
            if self.awaiting_post_release_observe {
                self.phase_event("first_matching_observe_after_release");
                self.awaiting_post_release_observe = false;
            }
            if let Some(key) = self.steps.front().and_then(|step| step.key) {
                let focus_access = key.focus_access_key_for(label);
                self.hit = None;
                self.last_hit = None;
                self.stable_hits = 0;
                self.keyboard_target_focused = response.has_focus();
                if self.awaiting_sample {
                    return false;
                }
                if self.response_visible == Some(false) {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                if clickable
                    && self.keyboard_pairs > 0
                    && (!key.traversal() || self.keyboard_cooldown == 0)
                    && (!key.access_key() || focus_access)
                    && (key.access_key() || response.has_focus())
                    && (!focus_access || response.has_focus())
                    && (!key.activation() || response.clicked())
                {
                    let event = serde_json::json!({
                        "button": label, "key": key, "key_pairs": self.keyboard_pairs,
                        "clicked": key.activation() && response.clicked(),
                        "focused": response.has_focus(), "changed": response.changed()
                    });
                    self.complete_event(event);
                    return true;
                }
                return false;
            }
            if self.steps.front().is_some_and(|step| step.expect_disabled) {
                self.hit = None;
                self.last_hit = None;
                self.stable_hits = 0;
                let whole_response = whole_response
                    || matches!(
                        label,
                        "Cursor appearance preview"
                            | "Windows cursor preview"
                            | "Visible cursor preview"
                            | "Hidden cursor preview"
                            | "Lock draft summary"
                            | "Lock target summary"
                            | "Lock candidate summary"
                            | "Lock region Left"
                            | "Lock region Top"
                            | "Lock region Right"
                            | "Lock region Bottom"
                            | "Annotation export region Left"
                            | "Annotation export region Top"
                            | "Annotation export region Right"
                            | "Annotation export region Bottom"
                            | "Apply Legacy bounds"
                            | "Apply Desktop bounds"
                            | "Apply Window bounds"
                            | "Apply Monitor bounds"
                            | "Apply Region bounds"
                    );
                let visible = if whole_response {
                    response.interact_rect.contains_rect(response.rect)
                } else {
                    response.interact_rect.contains(response.rect.center())
                };
                if !visible {
                    response.scroll_to_me(Some(egui::Align::Center));
                } else if !clickable
                    && (self.was_ready || self.bootstrap_shell)
                    && !self.awaiting_sample
                    && self.release.is_none()
                    && !self.awaiting_click
                    && self.steps.front().is_some_and(|step| {
                        self.started.elapsed() >= Duration::from_millis(step.wait_ms)
                    })
                {
                    self.complete_step(false);
                }
                // Even an unexpectedly enabled control must never receive a test click.
                return false;
            }
            // A real click can change layout before this response is observed.
            // Acknowledge it before using the new geometry to stage another click.
            if clickable && self.awaiting_click && response.clicked() {
                self.complete_step(true);
                return true;
            }
            if !clickable || self.response_visible != Some(true) {
                self.hit = None;
                self.last_hit = None;
                self.stable_hits = 0;
                if self.response_visible != Some(true) {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                return false;
            }
            let center = response.rect.center();
            self.stable_hits = if self
                .last_hit
                .is_some_and(|last| last.distance(center) < 0.5)
            {
                self.stable_hits.saturating_add(1)
            } else {
                1
            };
            self.last_hit = Some(center);
            self.hit = Some(center);
            self.diagnostic_hit = Some(center);
        }
        false
    }
    /// Record an Alt action only after the Draw page has actually staged or
    /// dispatched it. A visible response alone is not proof of activation.
    pub fn access_action(&mut self, label: &str) {
        if self.awaiting_sample
            || !self.io_gate()
            || self.keyboard_pairs == 0
            || self.response_enabled != Some(true)
        {
            return;
        }
        let Some(step) = self.steps.front() else {
            return;
        };
        if step.button != label
            || !matches!(
                step.key,
                Some(ScriptKey::AltD | ScriptKey::AltE | ScriptKey::AltC | ScriptKey::AltV)
            )
        {
            return;
        }
        let key = step.key;
        self.complete_event(serde_json::json!({
            "button": label,
            "key": key,
            "key_pairs": self.keyboard_pairs,
            "shortcut_activated": true
        }));
    }
    /// Complete a magnifier access-key step only after its selected user's
    /// draft checkbox actually toggles. Alt+B remains focus-only on Draw.
    pub fn magnifier_access_action(&mut self, label: &str) {
        let key = match label {
            "Show lens border" => ScriptKey::AltB,
            "Refresh automatically" => ScriptKey::AltU,
            _ => return,
        };
        self.complete_access_action(label, key);
    }
    /// Complete a Home access-key step only after its action is dispatched.
    pub fn home_access_action(&mut self, label: &str) {
        let key = match label {
            "Configure user" => ScriptKey::AltC,
            "Settings" => ScriptKey::AltS,
            "Hide" => ScriptKey::AltH,
            "Apps" => ScriptKey::AltA,
            _ => return,
        };
        self.complete_access_action(label, key);
    }
    fn complete_access_action(&mut self, label: &str, key: ScriptKey) {
        if self.awaiting_sample
            || !self.io_gate()
            || self.keyboard_pairs == 0
            || self.response_enabled != Some(true)
        {
            return;
        }
        let Some(step) = self.steps.front() else {
            return;
        };
        if step.button != label || step.key != Some(key) {
            return;
        }
        self.complete_event(serde_json::json!({
            "button": label,
            "key": key,
            "key_pairs": self.keyboard_pairs,
            "shortcut_activated": true
        }));
    }
    /// Alt+O completes only after its popup's editable color field has focus.
    pub fn access_focus(&mut self, label: &str, response: &egui::Response) {
        if self.awaiting_sample
            || !self.io_gate()
            || self.keyboard_pairs == 0
            || !response.has_focus()
        {
            return;
        }
        let Some(step) = self.steps.front() else {
            return;
        };
        if step.button != label || !matches!(step.key, Some(ScriptKey::AltO)) {
            return;
        }
        self.complete_event(serde_json::json!({
            "button": label,
            "key": ScriptKey::AltO,
            "key_pairs": self.keyboard_pairs,
            "focused": true,
            "color_editor_open": true
        }));
    }
    fn complete_step(&mut self, clicked: bool) {
        if let Some(step) = self.steps.front() {
            let event = if clicked {
                serde_json::json!({"button": step.button, "clicked": true})
            } else {
                serde_json::json!({
                    "button": step.button, "clicked": false, "enabled": false,
                    "visible": true, "observed_disabled": true
                })
            };
            self.complete_event(event);
        }
    }
    fn complete_event(&mut self, event: serde_json::Value) {
        let Some(line) = self.script_io.line_json(&event) else {
            return;
        };
        let Some(sequence) = self.script_io.append(&self.events_path, line) else {
            return;
        };
        self.pending_event_sequence = Some(sequence);
        self.last_event_sequence = Some(sequence);
    }
    fn finalize_event(&mut self) {
        if let Some(step) = self.steps.pop_front() {
            self.completed += 1;
            self.response_enabled = None;
            self.response_visible = None;
            self.response_label = None;
            self.awaiting_sample = step.wait_for_sample;
        }
        self.started = Instant::now();
        self.hit = None;
        self.last_hit = None;
        self.stable_hits = 0;
        self.awaiting_click = false;
        self.keyboard_pairs = 0;
        self.keyboard_cooldown = 0;
        self.keyboard_target_focused = false;
        self.diagnostic_hit = None;
    }
    /// Bind only after FrontendReady reaches the GUI, before Dashboard transfer.
    pub(crate) fn bind_capture_activity(&mut self, probe: &crate::gui_activity::Probe) {
        self.capture_activity = Some(probe.bind_render_capture(
            self.capture_script_id,
            self.capture_worker.is_some(),
            self.capture_worker.as_ref().map(CaptureWorker::observer),
        ));
    }
    pub fn capture(&mut self, ctx: &egui::Context, input: &egui::RawInput) {
        self.capture_at(ctx, input, Instant::now());
    }
    fn capture_at(&mut self, ctx: &egui::Context, input: &egui::RawInput, now: Instant) {
        self.diagnostic_frame = self.diagnostic_frame.saturating_add(1);
        let viewport = input.viewports.get(&input.viewport_id);
        self.latest_raw_viewport = serde_json::json!({
            "focused": input.focused,
            "viewport_id": format!("{:?}", input.viewport_id),
            "viewport": viewport.map(|info| serde_json::json!({
                "minimized": info.minimized,
                "maximized": info.maximized,
                "fullscreen": info.fullscreen,
                "focused": info.focused,
                "occluded": info.occluded,
                "close_requested": info.close_requested(),
                "inner_rect": info.inner_rect.map(|rect| [rect.min.x, rect.min.y,
                                                           rect.max.x, rect.max.y]),
                "outer_rect": info.outer_rect.map(|rect| [rect.min.x, rect.min.y,
                                                           rect.max.x, rect.max.y]),
                "native_pixels_per_point": info.native_pixels_per_point
            }))
        });
        let Some(worker) = &self.capture_worker else {
            return;
        };
        for event in &input.events {
            if let egui::Event::Screenshot {
                viewport_id,
                user_data,
                image,
            } = event
            {
                let ticket = user_data
                    .data
                    .as_ref()
                    .and_then(|data| data.downcast_ref::<Ticket>());
                if let Some(ticket) = ticket.filter(|ticket| {
                    ticket.script_id == self.capture_script_id
                        && ticket.viewport_id == input.viewport_id
                        && *viewport_id == input.viewport_id
                        && ticket.generation != 0
                        && ticket.generation <= self.capture_generation
                }) {
                    self.capture_received_generation =
                        self.capture_received_generation.max(ticket.generation);
                    let accepted = worker.submit(*ticket, image.clone());
                    if let Some(reporter) = &self.capture_activity {
                        reporter.received(ticket.generation, image.size, accepted, now);
                    }
                }
            }
        }
        if now.saturating_duration_since(self.last_capture) > Duration::from_secs(3) {
            self.capture_generation = self.capture_generation.saturating_add(1);
            if let Some(reporter) = &self.capture_activity {
                reporter.requested(self.capture_generation, now);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                Ticket {
                    script_id: self.capture_script_id,
                    viewport_id: input.viewport_id,
                    generation: self.capture_generation,
                    requested_at: SystemTime::now(),
                },
            )));
            self.last_capture = now;
        }
    }
    pub fn input(&mut self, input: &mut egui::RawInput) {
        // Explicit test mode isolates the dashboard's virtual pointer from desktop events.
        input.focused = true;
        input.events.retain(|e| {
            !matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerGone
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::MouseMoved(_)
                    | egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::WindowFocused(_)
            )
        });
        if self.io_fault().is_some() {
            if self.release.take().is_some() {
                // Cancel an in-flight virtual press outside the viewport, without clicking.
                let cancelled = egui::pos2(-1_000_000.0, -1_000_000.0);
                input.events.push(egui::Event::PointerMoved(cancelled));
                input.events.push(egui::Event::PointerButton {
                    pos: cancelled,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
                input.events.push(egui::Event::PointerGone);
            }
            return;
        }
        if let Some(sequence) = self.pending_event_sequence {
            if !self.script_io.allows(Some(sequence)) {
                return;
            }
            self.pending_event_sequence = None;
            self.finalize_event();
        }
        if !self.io_gate() {
            return;
        }
        if self.awaiting_sample {
            self.script_io.request_sample(self.completed);
            let sampled = self.script_io.status().sampled;
            if sampled.is_none_or(|count| count < self.completed) {
                return;
            }
            self.awaiting_sample = false;
            self.script_io.clear_sample();
            self.started = Instant::now();
            self.phase_event("sample_ack");
            return;
        }
        if let Some(step) = self.steps.front()
            && let Some(key) = step.key
        {
            // Opt-in fixture only: ignore physical modifiers, and never send OS input.
            input.modifiers = egui::Modifiers::NONE;
            if self.keyboard_cooldown > 0 {
                self.keyboard_cooldown -= 1;
                return;
            }
            let may_send = if key.traversal() {
                self.keyboard_pairs < 24
            } else {
                self.keyboard_pairs == 0 && (key.access_key() || self.keyboard_target_focused)
            };
            if self.was_ready
                && self.response_enabled == Some(true)
                && self.response_visible == Some(true)
                && self.started.elapsed() >= Duration::from_millis(step.wait_ms)
                && may_send
            {
                let modifiers = egui::Modifiers {
                    shift: matches!(key, ScriptKey::ShiftTab),
                    alt: key.access_key(),
                    ..egui::Modifiers::NONE
                };
                input.modifiers = modifiers;
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key {
                        key: key.egui(),
                        physical_key: Some(key.egui()),
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
                self.keyboard_pairs += 1;
                self.keyboard_cooldown = 2;
            }
            return;
        }
        if let Some(pos) = self.release.take() {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            self.awaiting_click = true;
            self.awaiting_post_release_observe = true;
            self.phase_event("injected_pointer_release");
            return;
        }
        if self.awaiting_click {
            self.awaiting_click = false;
            return;
        }
        let hit = self.hit.take();
        if self.steps.front().is_some_and(|s| {
            !s.expect_disabled && self.started.elapsed() >= Duration::from_millis(s.wait_ms)
        }) && self.stable_hits >= 3
            && let Some(pos) = hit
        {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            self.release = Some(pos);
            self.phase_event("injected_pointer_press");
        }
    }

    fn phase_event(&mut self, phase: &str) {
        let step_index = self.completed;
        if self.phase_keys.len() >= 64 || !self.phase_keys.insert((step_index, phase.to_owned())) {
            return;
        }
        let event = serde_json::json!({
            "monotonic_ms": self.diagnostic_started.elapsed().as_millis(),
            "frame": self.diagnostic_frame,
            "step_index": step_index,
            "step_label": self.steps.front().map(|step| step.button.as_str()),
            "phase": phase,
            "awaiting_sample": self.awaiting_sample,
            "awaiting_click": self.awaiting_click,
            "awaiting_release": self.release.is_some(),
            "script_hit": self.diagnostic_hit.map(|point| [point.x, point.y]),
            "response": self.latest_response,
            "raw_viewport": self.latest_raw_viewport
        });
        let Some(line) = self.script_io.line_json(&event) else {
            return;
        };
        self.pending_phase_sequence = self.script_io.append(&self.phase_log_path, line);
    }
}
