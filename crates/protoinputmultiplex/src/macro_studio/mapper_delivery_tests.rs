// Included inside macro_studio::tests to reuse the existing supervised fixtures.
struct MapperAfterDownEngine<'a> {
    inner: &'a mut MockEngine,
    fail_key: u32,
    strict: bool,
    scopes: Vec<bool>,
    text: Option<MapperTextScenario>,
}
#[derive(Default)]
struct MapperTextScenario {
    recipient: Option<(usize, Window)>,
    delivered: Vec<(Window, u16)>,
    began: usize,
    ended: usize,
    fail_after: Option<usize>,
    switch_after_first: Option<Window>,
    exit_after_first: bool,
}
impl Engine for MapperAfterDownEngine<'_> {
    fn begin_text_delivery(&mut self, user: usize) -> Result<()> {
        if let Some(text) = &mut self.text {
            ensure!(text.recipient.is_none(), "Nested test text scope");
            text.recipient = Some((
                user,
                self.inner
                    .keyboard_window(user)?
                    .context("No text recipient")?,
            ));
            text.began += 1;
        }
        Ok(())
    }
    fn end_text_delivery(&mut self, user: usize) {
        if let Some(text) = &mut self.text {
            assert_eq!(text.recipient.take().map(|(user, _)| user), Some(user));
            text.ended += 1;
        }
    }
    fn mapper_delivery(&mut self, active: bool) -> Result<()> {
        self.strict = active;
        self.scopes.push(active);
        Ok(())
    }
    fn forwarding_supported(&self) -> bool {
        self.inner.forwarding_supported()
    }
    fn pending_attachment_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.pending_attachment_window(user)
    }
    fn keyboard_enabled(&self, user: usize) -> bool {
        self.inner.keyboard_enabled(user)
    }
    fn pointer(&self, user: usize) -> Result<(i32, i32)> {
        self.inner.pointer(user)
    }
    fn selected_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.selected_window(user)
    }
    fn keyboard_window(&self, user: usize) -> Result<Option<Window>> {
        if let Some(text) = &self.text
            && let Some((retained_user, window)) = text.recipient
        {
            ensure!(retained_user == user, "Wrong retained text user");
            ensure!(
                !text.exit_after_first || text.delivered.is_empty(),
                "Retained text recipient exited"
            );
            return Ok(Some(window));
        }
        self.inner.keyboard_window(user)
    }
    fn mouse_window(&self, user: usize) -> Result<Option<Window>> {
        self.inner.mouse_window(user)
    }
    fn movement_window(&self, user: usize, x: i32, y: i32) -> Result<Option<Window>> {
        self.inner.movement_window(user, x, y)
    }
    fn packet_window(&self, user: usize, packet: Packet) -> Result<Option<Window>> {
        if packet.kind == 0 {
            self.inner.mouse_window(user)
        } else {
            self.keyboard_window(user)
        }
    }
    fn input(&mut self, packet: Packet) -> Result<()> {
        if packet.kind == 2
            && let Some(text) = &mut self.text
        {
            let (_, window) = text
                .recipient
                .context("Text packet escaped recipient scope")?;
            ensure!(
                text.fail_after
                    .is_none_or(|limit| text.delivered.len() < limit),
                "Controlled partial text failure"
            );
            self.inner.input(packet)?;
            text.delivered.push((window, packet.data as u16));
            if text.delivered.len() == 1
                && let Some(next) = text.switch_after_first
            {
                self.inner.keyboard_override = Some(next);
            }
            return Ok(());
        }
        self.inner.input(packet)?;
        ensure!(
            !(self.strict
                && packet.kind == 1
                && packet.flags & 1 == 0
                && packet.data == self.fail_key),
            "controlled failure after native DOWN was retained"
        );
        Ok(())
    }
    fn key_held(&self, device: u32, key: u32) -> bool {
        self.inner.key_held(device, key)
    }
    fn button_held(&self, device: u32, button: u32) -> bool {
        self.inner.button_held(device, button)
    }
    fn note_mouse_activity(&mut self, user: usize) -> Result<()> {
        self.inner.note_mouse_activity(user)
    }
    fn paused(&self) -> bool {
        self.inner.paused()
    }
    fn release_all(&mut self) -> Result<()> {
        self.inner.release_all()
    }
    fn pause(&mut self, value: bool) -> Result<()> {
        self.inner.pause(value)
    }
    fn settings(&mut self, user: usize, settings: CursorSettings) -> Result<()> {
        self.inner.settings(user, settings)
    }
    fn tick(&mut self) -> Result<(bool, Vec<RoutingState>)> {
        self.inner.tick()
    }
    fn move_to(&mut self, user: usize, x: i32, y: i32) -> Result<()> {
        self.inner.move_to(user, x, y)
    }
}

fn mapper_delivery_fixture() -> (Runtime, MockEngine, [u32; 2], [u32; 2]) {
    let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
    // Fake provider capability derived from this fixture's known virtual devices.
    // Production derives this independently from persisted virtual:// paths.
    runtime.mapper_virtual_users.fill(true);
    runtime.agent_access = Arc::new(AgentAccess {
        enabled: true,
        allowed_pids: BTreeSet::from([101]),
        users: runtime
            .seats
            .iter()
            .enumerate()
            .map(|(user, seat)| {
                (
                    user,
                    AgentGrant {
                        mouse: seat.mouse,
                        keyboard: seat.keyboard,
                    },
                )
            })
            .collect(),
        session_control: true,
        active_users: BTreeSet::from([0, 1]),
        ..Default::default()
    });
    engine.window = Some(Window { hwnd: 10, pid: 101 });
    (runtime, engine, mice, keyboards)
}
fn mapper_delivery_arm(runtime: &mut Runtime, engine: &mut dyn Engine) {
    let definition: mux_core::mapper::Definition = serde_json::from_value(serde_json::json!({
        "version": 1,
        "nodes": [
            {"id":"source", "kind":{"type":"key", "user":runtime.identities[0], "key":65, "disposition":"consume"}},
            {"id":"sink", "kind":{"type":"sink", "user":runtime.identities[1], "channel":{"type":"key_hold", "key":66}}}
        ],
        "edges": [{"from":{"node":"source","port":"value"},"to":{"node":"sink","port":"value"}}]
    })).unwrap();
    let applied = runtime
        .command(
            automation_runtime::Command::Mapper(crate::mapper_runtime::Command::Apply {
                definition,
                expected_revision: 0,
                gui: None,
            }),
            engine,
        )
        .unwrap();
    assert!(applied.mapper.error.is_none(), "{:?}", applied.mapper.error);
    let armed = runtime
        .command(
            automation_runtime::Command::Mapper(crate::mapper_runtime::Command::Arm {
                expected_revision: 1,
                gui: None,
            }),
            engine,
        )
        .unwrap();
    assert!(armed.mapper.error.is_none(), "{:?}", armed.mapper.error);
    assert!(armed.mapper.armed);
}

#[test]
fn mapper_ocr_graph_requires_separate_capture_permission_before_passthrough_and_tracks_read_user() {
    let (mut runtime, mut engine, mice, _) = mapper_delivery_fixture();
    runtime.mapper_capture_pids = BTreeSet::from([101]);
    Arc::make_mut(&mut runtime.agent_access).clients.insert(7);
    let definition = serde_json::from_value(serde_json::json!({"version":1,"nodes":[
        {"id":"wheel","kind":{"type":"wheel","user":runtime.identities[0],"horizontal":false}},
        {"id":"pulse","kind":{"type":"impulse_to_pulse"}},
        {"id":"ocr","kind":{"type":"ocr","capture_user":runtime.identities[1],"region":null}},
        {"id":"text","kind":{"type":"monitor","signal":"text"}}
    ],"edges":[
        {"from":{"node":"wheel","port":"value"},"to":{"node":"pulse","port":"value"}},
        {"from":{"node":"pulse","port":"value"},"to":{"node":"ocr","port":"trigger"}},
        {"from":{"node":"ocr","port":"text"},"to":{"node":"text","port":"value"}}
    ]}))
    .unwrap();
    for command in [
        crate::mapper_runtime::Command::Apply {
            definition,
            expected_revision: 0,
            gui: None,
        },
        crate::mapper_runtime::Command::Arm {
            expected_revision: 1,
            gui: None,
        },
    ] {
        let reply = runtime
            .command(automation_runtime::Command::Mapper(command), &mut engine)
            .unwrap();
        assert!(reply.mapper.error.is_none(), "{:?}", reply.mapper.error);
    }
    assert!(runtime.state(false).mapper.armed);
    assert!(
        runtime.require_selection_idle(1).is_err(),
        "explicit OCR read user is part of armed mapper scope"
    );
    let error = runtime
        .agent_action(
            7,
            Action::Packet {
                user: 0,
                packet: Packet::wheel(mice[0], 120),
            },
            &mut engine,
        )
        .unwrap_err();
    assert!(error.to_string().contains("capture permission"), "{error}");
    assert!(
        engine.packets.is_empty(),
        "capture denial precedes original passthrough delivery"
    );
    assert_eq!(runtime.state(false).mapper.pending_observations, 0);
    runtime.retire_mapper_for_user(1, &mut engine).unwrap();
    assert!(!runtime.state(false).mapper.armed);
    runtime.require_selection_idle(1).unwrap();
}

#[test]
fn mapper_adapter_partial_down_retains_up_and_resets_strict_scope_after_errors() {
    let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
    mapper_delivery_arm(&mut runtime, &mut engine);
    let unrelated = keyboard_test_key(keyboards[1], 0x43, false);
    runtime
        .claims
        .input(Origin::Manual(1), unrelated, &mut engine)
        .unwrap();
    engine.fail_key_releases.insert((keyboards[1], 0x42));
    let mut wrapper = MapperAfterDownEngine {
        inner: &mut engine,
        fail_key: 0x42,
        strict: false,
        scopes: Vec::new(),
        text: None,
    };
    let error = runtime
        .agent_action(
            7,
            Action::Packet {
                user: 0,
                packet: keyboard_test_key(keyboards[0], 0x41, false),
            },
            &mut wrapper,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("controlled failure after native DOWN")
    );
    assert!(
        !wrapper.strict,
        "failed delivery and failed retirement must each restore ordinary routing policy"
    );
    assert_eq!(wrapper.scopes.last(), Some(&false));
    assert!(wrapper.key_held(keyboards[1], 0x42));
    assert!(wrapper.key_held(keyboards[1], 0x43));
    let state = runtime.state(false).mapper;
    assert!(!state.armed && state.cleanup_pending);
    assert_eq!(state.actor_contexts, 0);
    assert!(
        runtime.claims.1.keys().any(|owner| matches!(
            owner.origin,
            Origin::Mapper(crate::routing_engine::ProgramOrigin::Agent(7, 0), 1, _)
        )),
        "uncertain DOWN must retain its mapper owner"
    );
    wrapper.inner.fail_key_releases.clear();
    runtime
        .with_mapper(&mut wrapper, false, |mapper, backend| {
            mapper.retire(backend)
        })
        .unwrap();
    assert!(!wrapper.strict);
    assert!(!wrapper.key_held(keyboards[1], 0x42));
    assert!(
        wrapper.key_held(keyboards[1], 0x43),
        "mapper retirement must preserve ordinary holds"
    );
    assert!(!runtime.state(false).mapper.cleanup_pending);
    // The consumed raw DOWN remains suppressed after retirement until actual UP.
    assert_eq!(runtime.claims.3.len(), 1);
    runtime
        .agent_action(
            7,
            Action::Packet {
                user: 0,
                packet: keyboard_test_key(keyboards[0], 0x41, true),
            },
            &mut wrapper,
        )
        .unwrap();
    assert!(runtime.claims.3.is_empty());
    assert!(!wrapper.key_held(keyboards[0], 0x41));
}

#[test]
fn mapper_adapter_eof_releases_only_one_actor_and_preserves_shared_native_hold() {
    let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
    mapper_delivery_arm(&mut runtime, &mut engine);
    runtime
        .claims
        .input(
            Origin::Manual(1),
            keyboard_test_key(keyboards[1], 0x43, false),
            &mut engine,
        )
        .unwrap();
    for client in [7, 8] {
        runtime
            .agent_action(
                client,
                Action::Packet {
                    user: 0,
                    packet: keyboard_test_key(keyboards[0], 0x41, false),
                },
                &mut engine,
            )
            .unwrap();
    }
    assert_eq!(
        engine
            .packets
            .iter()
            .filter(|packet| packet.device == keyboards[1]
                && packet.data == 0x42
                && packet.flags & 1 == 0)
            .count(),
        1
    );
    assert_eq!(runtime.claims.3.len(), 2);
    runtime.cancel_agent(7, &mut engine).unwrap();
    assert_eq!(runtime.agent_pending_owners(7), 0);
    assert_eq!(runtime.agent_pending_owners(8), 1);
    assert!(runtime.state(false).mapper.armed);
    assert_eq!(runtime.state(false).mapper.actor_contexts, 1);
    assert!(engine.key_held(keyboards[1], 0x42));
    assert!(engine.key_held(keyboards[1], 0x43));
    assert_eq!(runtime.claims.3.len(), 1);
    runtime.cancel_agent(8, &mut engine).unwrap();
    assert!(!engine.key_held(keyboards[1], 0x42));
    assert!(engine.key_held(keyboards[1], 0x43));
    assert!(runtime.claims.3.is_empty());
}

#[test]
fn mapper_adapter_missing_or_ungranted_recipient_rejects_before_consuming_original() {
    for recipient in [None, Some(Window { hwnd: 20, pid: 202 })] {
        let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
        mapper_delivery_arm(&mut runtime, &mut engine);
        engine.window = recipient;
        let before = engine.packets.len();
        assert!(
            runtime
                .agent_action(
                    7,
                    Action::Packet {
                        user: 0,
                        packet: keyboard_test_key(keyboards[0], 0x41, false)
                    },
                    &mut engine
                )
                .is_err()
        );
        assert_eq!(engine.packets.len(), before);
        assert!(
            runtime.claims.0.is_empty()
                && runtime.claims.1.is_empty()
                && runtime.claims.3.is_empty()
        );
        assert_eq!(
            runtime.state(false).mapper.actor_contexts,
            0,
            "rejected wave must not latch actor values"
        );
    }
}

fn mapper_text_arm(runtime: &mut Runtime, engine: &mut dyn Engine, text: &str, extra_pulse: bool) {
    let mut nodes = vec![
        serde_json::json!({"id":"source","kind":{"type":"key","user":runtime.identities[0],"key":65,"disposition":"consume"}}),
        serde_json::json!({"id":"pulse","kind":{"type":"rising_edge"}}),
        serde_json::json!({"id":"text","kind":{"type":"constant","value":{"type":"text","value":text}}}),
        serde_json::json!({"id":"sink","kind":{"type":"sink","user":runtime.identities[1],"channel":{"type":"type_text"}}}),
        serde_json::json!({"id":"hold-source","kind":{"type":"key","user":runtime.identities[0],"key":67,"disposition":"consume"}}),
        serde_json::json!({"id":"hold-sink","kind":{"type":"sink","user":runtime.identities[1],"channel":{"type":"key_hold","key":66}}}),
    ];
    let mut edges = vec![
        serde_json::json!({"from":{"node":"source","port":"value"},"to":{"node":"pulse","port":"value"}}),
        serde_json::json!({"from":{"node":"pulse","port":"value"},"to":{"node":"sink","port":"trigger"}}),
        serde_json::json!({"from":{"node":"text","port":"value"},"to":{"node":"sink","port":"text"}}),
        serde_json::json!({"from":{"node":"hold-source","port":"value"},"to":{"node":"hold-sink","port":"value"}}),
    ];
    if extra_pulse {
        nodes.push(serde_json::json!({"id":"also","kind":{"type":"sink","user":runtime.identities[1],"channel":{"type":"key_pulse","key":68}}}));
        edges.push(serde_json::json!({"from":{"node":"pulse","port":"value"},"to":{"node":"also","port":"value"}}));
    }
    let definition =
        serde_json::from_value(serde_json::json!({"version":1,"nodes":nodes,"edges":edges}))
            .unwrap();
    for command in [
        crate::mapper_runtime::Command::Apply {
            definition,
            expected_revision: 0,
            gui: None,
        },
        crate::mapper_runtime::Command::Arm {
            expected_revision: 1,
            gui: None,
        },
    ] {
        let state = runtime
            .command(automation_runtime::Command::Mapper(command), engine)
            .unwrap();
        assert!(state.mapper.error.is_none(), "{:?}", state.mapper.error);
    }
    assert!(runtime.state(false).mapper.armed);
}

fn mapper_text_key(
    runtime: &mut Runtime,
    engine: &mut dyn Engine,
    keyboard: u32,
    key: u32,
    up: bool,
) -> Result<()> {
    runtime
        .agent_action(
            7,
            Action::Packet {
                user: 0,
                packet: keyboard_test_key(keyboard, key, up),
            },
            engine,
        )
        .map(|_| ())
}

#[test]
fn mapper_type_text_pins_unicode_recipient_without_creating_held_text_claims() {
    let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
    mapper_text_arm(&mut runtime, &mut engine, "Ω🚀x", false);
    let original = engine.window.unwrap();
    let mut wrapper = MapperAfterDownEngine {
        inner: &mut engine,
        fail_key: 0,
        strict: false,
        scopes: Vec::new(),
        text: Some(MapperTextScenario {
            switch_after_first: Some(Window { hwnd: 20, pid: 202 }),
            ..Default::default()
        }),
    };
    mapper_text_key(&mut runtime, &mut wrapper, keyboards[0], 65, false).unwrap();
    let text = wrapper.text.as_ref().unwrap();
    assert_eq!(
        text.delivered,
        "Ω🚀x"
            .encode_utf16()
            .map(|unit| (original, unit))
            .collect::<Vec<_>>()
    );
    assert_eq!((text.began, text.ended, text.recipient), (1, 1, None));
    assert!(!wrapper.strict);
    assert!(
        runtime.claims.1.is_empty(),
        "instantaneous text creates no held owner"
    );
    assert!(wrapper.inner.keys.is_empty());
    assert!(
        runtime.require_selection_idle(1).is_err(),
        "armed graph keeps the existing selection guard"
    );
    let revision = runtime.state(false).mapper.revision;
    let state = runtime
        .command(
            automation_runtime::Command::Mapper(crate::mapper_runtime::Command::Disarm {
                expected_revision: revision,
                gui: None,
            }),
            &mut wrapper,
        )
        .unwrap();
    assert!(state.mapper.error.is_none() && !state.mapper.armed);
    runtime.require_selection_idle(1).unwrap();
    runtime.cancel_agent(7, &mut wrapper).unwrap();
    assert_eq!(
        wrapper.text.as_ref().unwrap().delivered.len(),
        4,
        "EOF cannot replay text"
    );
}

#[test]
fn mapper_type_text_partial_failure_or_exit_retires_claims_without_replaying_prefix() {
    for (exit, fail_release) in [(false, false), (false, true), (true, false)] {
        let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
        mapper_text_arm(&mut runtime, &mut engine, "ABC", false);
        runtime
            .claims
            .input(
                Origin::Manual(1),
                keyboard_test_key(keyboards[1], 68, false),
                &mut engine,
            )
            .unwrap();
        mapper_text_key(&mut runtime, &mut engine, keyboards[0], 67, false).unwrap();
        assert!(engine.key_held(keyboards[1], 66));
        if fail_release {
            engine.fail_key_releases.insert((keyboards[1], 66));
        }
        let mut wrapper = MapperAfterDownEngine {
            inner: &mut engine,
            fail_key: 0,
            strict: false,
            scopes: Vec::new(),
            text: Some(MapperTextScenario {
                fail_after: (!exit).then_some(1),
                exit_after_first: exit,
                ..Default::default()
            }),
        };
        assert!(mapper_text_key(&mut runtime, &mut wrapper, keyboards[0], 65, false).is_err());
        let text = wrapper.text.as_ref().unwrap();
        assert_eq!(
            text.delivered
                .iter()
                .map(|(_, unit)| *unit)
                .collect::<Vec<_>>(),
            vec![65]
        );
        assert_eq!((text.began, text.ended, text.recipient), (1, 1, None));
        assert!(!wrapper.strict);
        let state = runtime.state(false).mapper;
        assert!(!state.armed && state.actor_contexts == 0);
        assert_eq!(state.cleanup_pending, fail_release);
        assert_eq!(wrapper.key_held(keyboards[1], 66), fail_release);
        assert!(
            wrapper.key_held(keyboards[1], 68),
            "ordinary hold must survive Mapper retirement"
        );
        wrapper.inner.fail_key_releases.clear();
        runtime
            .with_mapper(&mut wrapper, false, |mapper, backend| {
                mapper.retire(backend)
            })
            .unwrap();
        assert!(!wrapper.key_held(keyboards[1], 66));
        assert!(wrapper.key_held(keyboards[1], 68));
        assert!(!runtime.state(false).mapper.cleanup_pending);
        runtime.cancel_agent(7, &mut wrapper).unwrap();
        assert_eq!(
            wrapper.text.as_ref().unwrap().delivered.len(),
            1,
            "retirement and EOF must not retry a typed prefix"
        );
    }
}

#[test]
fn mapper_type_text_invalid_or_overbudget_wave_has_no_input_or_claim_side_effect() {
    for (text, extra_pulse) in [
        ("x".repeat(129), false),
        ("x".repeat(127), true),
        ("A\rB".into(), false),
    ] {
        let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
        mapper_text_arm(&mut runtime, &mut engine, &text, extra_pulse);
        assert!(mapper_text_key(&mut runtime, &mut engine, keyboards[0], 65, false).is_err());
        assert!(
            engine.packets.is_empty(),
            "full validation precedes the original and every sink"
        );
        assert!(
            runtime.claims.0.is_empty()
                && runtime.claims.1.is_empty()
                && runtime.claims.3.is_empty()
        );
        assert_eq!(runtime.state(false).mapper.actor_contexts, 0);
    }
}

#[test]
fn mapper_type_text_whole_wave_bound_counts_units_and_empty_is_noop() {
    for (text, extra_pulse, expected_units, expected_keys) in [
        ("🚀".repeat(64), false, 128, 0),
        ("x".repeat(126), true, 126, 2),
        (String::new(), false, 0, 0),
    ] {
        let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
        mapper_text_arm(&mut runtime, &mut engine, &text, extra_pulse);
        mapper_text_key(&mut runtime, &mut engine, keyboards[0], 65, false).unwrap();
        assert_eq!(
            engine.packets.iter().filter(|p| p.kind == 2).count(),
            expected_units
        );
        assert_eq!(
            engine.packets.iter().filter(|p| p.kind == 1).count(),
            expected_keys
        );
        assert!(engine.keys.is_empty());
        assert!(runtime.claims.1.is_empty());
        assert_eq!(runtime.state(false).mapper.actor_contexts, 1);
    }
}

#[test]
fn mapper_type_text_rechecks_output_authority_before_consuming_source() {
    for deny in 0..4 {
        let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
        mapper_text_arm(&mut runtime, &mut engine, "ABC", false);
        match deny {
            0 => engine.window = None,
            1 => engine.window = Some(Window { hwnd: 20, pid: 202 }),
            2 => {
                engine.disabled_keyboard_users.insert(1);
            }
            _ => {
                Arc::make_mut(&mut runtime.agent_access).users.remove(&1);
            }
        }
        assert!(mapper_text_key(&mut runtime, &mut engine, keyboards[0], 65, false).is_err());
        assert!(engine.packets.is_empty());
        assert!(
            runtime.claims.0.is_empty()
                && runtime.claims.1.is_empty()
                && runtime.claims.3.is_empty()
        );
        assert_eq!(runtime.state(false).mapper.actor_contexts, 0);
    }
}
