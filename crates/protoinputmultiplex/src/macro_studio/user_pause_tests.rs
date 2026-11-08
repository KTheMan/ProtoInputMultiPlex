#[test]
fn user_pause_isolates_actors_and_quarantines_until_real_up() {
    let (mut runtime, mut engine, mice, keyboards) = two_user_runtime_and_engine();
    let a = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, false),
    };
    let b = Action::Packet {
        user: 1,
        packet: keyboard_test_key(keyboards[1], 0x42, false),
    };
    runtime.action(a.clone(), &mut engine, true).unwrap();
    runtime.action(b.clone(), &mut engine, true).unwrap();
    runtime.user_pause(0, true, &mut engine).unwrap();
    assert!(!engine.key_held(keyboards[0], 0x41));
    assert!(engine.key_held(keyboards[1], 0x42));
    let after = engine.packets.len();
    for action in [
        a.clone(),
        Action::MoveTo {
            user: 0,
            x: 50,
            y: 80,
        },
        Action::Text {
            user: 0,
            text: "discard".into(),
        },
        Action::Packet {
            user: 0,
            packet: Packet::wheel(mice[0], 120),
        },
    ] {
        runtime.action(action, &mut engine, true).unwrap();
    }
    assert_eq!(engine.packets.len(), after);
    runtime.user_pause(0, false, &mut engine).unwrap();
    runtime.action(a.clone(), &mut engine, true).unwrap();
    assert_eq!(engine.packets.len(), after);
    let up = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, true),
    };
    runtime.action(up, &mut engine, true).unwrap();
    runtime.action(a, &mut engine, true).unwrap();
    assert!(engine.key_held(keyboards[0], 0x41));
    assert!(engine.key_held(keyboards[1], 0x42));
}

#[test]
fn user_pause_survives_prior_global_cancel_and_global_resume_does_not_replay() {
    let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
    let down = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, false),
    };
    runtime.action(down.clone(), &mut engine, true).unwrap();
    runtime.cancel(&mut engine).unwrap();
    engine.pause(true).unwrap();
    runtime.user_pause(0, true, &mut engine).unwrap();
    assert!(runtime.user_pause(0, false, &mut engine).is_err());
    engine.pause(false).unwrap();
    runtime.user_pause(0, false, &mut engine).unwrap();
    let after = engine.packets.len();
    runtime.action(down.clone(), &mut engine, true).unwrap();
    assert_eq!(engine.packets.len(), after);
    runtime
        .action(
            Action::Packet {
                user: 0,
                packet: keyboard_test_key(keyboards[0], 0x41, true),
            },
            &mut engine,
            true,
        )
        .unwrap();
    runtime.action(down, &mut engine, true).unwrap();
    assert!(engine.key_held(keyboards[0], 0x41));
}

#[test]
fn user_pause_failed_release_stays_pending_without_cancelling_other_user() {
    let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
    for (user, key) in [(0, 0x41), (1, 0x42)] {
        runtime
            .action(
                Action::Packet {
                    user,
                    packet: keyboard_test_key(keyboards[user], key, false),
                },
                &mut engine,
                true,
            )
            .unwrap();
    }
    engine.fail_key_releases.insert((keyboards[0], 0x41));
    assert!(runtime.user_pause(0, true, &mut engine).is_err());
    let state = runtime.user_pause_state(0, &engine);
    assert!(state.paused);
    assert_eq!(state.pending_releases, 1);
    assert!(runtime.user_pause(0, false, &mut engine).is_err());
    assert!(engine.key_held(keyboards[1], 0x42));
    engine.fail_key_releases.clear();
    runtime.user_pause(0, true, &mut engine).unwrap();
    runtime.user_pause(0, false, &mut engine).unwrap();
    assert!(engine.key_held(keyboards[1], 0x42));
    assert!(!engine.key_held(keyboards[0], 0x41));
    assert_eq!(engine.release_all_attempts, 0);
}

#[test]
fn user_pause_blocks_affected_automation_before_mutation() {
    let (mut runtime, mut engine, _, _) = two_user_runtime_and_engine();
    runtime.user_pause(0, true, &mut engine).unwrap();
    let before = runtime.macro_revision();
    assert!(
        runtime
            .command(automation_runtime::Command::RecordStart, &mut engine)
            .is_err()
    );
    assert!(
        runtime
            .studio_command(
                Command::Record {
                    user: 0,
                    active: true
                },
                &mut engine
            )
            .is_err()
    );
    assert_eq!(runtime.macro_revision(), before);
    // An independent program/recording on B remains available.
    runtime
        .studio_command(
            Command::Record {
                user: 1,
                active: true,
            },
            &mut engine,
        )
        .unwrap();
    assert!(runtime.studio_state(None).users[1].recording);
    runtime.user_pause(0, false, &mut engine).unwrap();
    assert!(runtime.studio_state(None).users[1].recording);
}

#[test]
fn user_pause_denied_actor_does_not_consume_another_actors_quarantine() {
    let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
    let down = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, false),
    };
    runtime.action(down.clone(), &mut engine, true).unwrap();
    runtime.user_pause(0, true, &mut engine).unwrap();
    let count = runtime.sources.down.len();
    let up = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, true),
    };
    assert!(runtime.agent_action(99, up, &mut engine).is_err());
    assert_eq!(runtime.sources.down.len(), count);
    runtime.user_pause(0, false, &mut engine).unwrap();
    let after = engine.packets.len();
    runtime.action(down, &mut engine, true).unwrap();
    assert_eq!(engine.packets.len(), after);
}

#[test]
fn user_pause_client_eof_and_revoked_up_preserve_other_client_quarantine() {
    let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
    let down = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, false),
    };
    for client in [7, 8] {
        runtime
            .agent_action(client, down.clone(), &mut engine)
            .unwrap();
    }
    runtime.user_pause(0, true, &mut engine).unwrap();
    assert_eq!(runtime.sources.down.len(), 2);
    let grant = Arc::make_mut(&mut runtime.agent_access)
        .users
        .remove(&0)
        .unwrap();
    assert!(
        runtime
            .agent_action(
                8,
                Action::Packet {
                    user: 0,
                    packet: keyboard_test_key(keyboards[0], 0x41, true)
                },
                &mut engine
            )
            .is_err()
    );
    assert_eq!(runtime.sources.down.len(), 2);
    Arc::make_mut(&mut runtime.agent_access)
        .users
        .insert(0, grant);
    runtime.cancel_agent(7, &mut engine).unwrap();
    assert_eq!(runtime.sources.down.len(), 1);
    assert!(
        runtime
            .sources
            .down
            .keys()
            .all(|owner| owner.origin == Origin::Agent(8, 0))
    );
    runtime.user_pause(0, false, &mut engine).unwrap();
    let after = engine.packets.len();
    runtime.agent_action(8, down, &mut engine).unwrap();
    assert_eq!(engine.packets.len(), after);
}

#[test]
fn user_pause_resume_mapper_arm_waits_for_quarantined_source_up() {
    let (mut runtime, mut engine, _, keyboards) = mapper_delivery_fixture();
    let down = Action::Packet {
        user: 0,
        packet: keyboard_test_key(keyboards[0], 0x41, false),
    };
    runtime.action(down, &mut engine, true).unwrap();
    runtime.user_pause(0, true, &mut engine).unwrap();
    runtime.user_pause(0, false, &mut engine).unwrap();
    let definition = serde_json::from_value(serde_json::json!({
        "version":1,
        "nodes":[{"id":"source","kind":{"type":"key","user":runtime.identities[0],"key":65,"disposition":"consume"}},
            {"id":"sink","kind":{"type":"sink","user":runtime.identities[1],"channel":{"type":"key_hold","key":66}}}],
        "edges":[{"from":{"node":"source","port":"value"},"to":{"node":"sink","port":"value"}}]
    })).unwrap();
    let applied = runtime
        .command(
            automation_runtime::Command::Mapper(crate::mapper_runtime::Command::Apply {
                definition,
                expected_revision: 0,
                gui: None,
            }),
            &mut engine,
        )
        .unwrap();
    assert!(applied.error.is_none());
    let arm = || {
        automation_runtime::Command::Mapper(crate::mapper_runtime::Command::Arm {
            expected_revision: 1,
            gui: None,
        })
    };
    let rejected = runtime.command(arm(), &mut engine).unwrap();
    assert!(rejected.error.is_some());
    assert!(!rejected.mapper.armed);
    runtime
        .action(
            Action::Packet {
                user: 0,
                packet: keyboard_test_key(keyboards[0], 0x41, true),
            },
            &mut engine,
            true,
        )
        .unwrap();
    let accepted = runtime.command(arm(), &mut engine).unwrap();
    assert!(accepted.error.is_none(), "{:?}", accepted.error);
    assert!(accepted.mapper.armed);
}

#[test]
fn user_pause_preserves_other_users_running_independent_program() {
    let (mut runtime, mut engine, _, keyboards) = two_user_runtime_and_engine();
    runtime
        .studio_command(
            Command::Load {
                user: 1,
                clip: key_clip(1, keyboards[1]),
            },
            &mut engine,
        )
        .unwrap();
    runtime
        .studio_command(
            Command::Play {
                user: 1,
                options: PlaybackOptions::default(),
            },
            &mut engine,
        )
        .unwrap();
    runtime.tick(&mut engine).unwrap();
    assert!(engine.key_held(keyboards[1], 0x41));
    runtime.user_pause(0, true, &mut engine).unwrap();
    runtime.tick(&mut engine).unwrap();
    assert!(engine.key_held(keyboards[1], 0x41));
    assert_eq!(
        runtime.studio_state(None).users[1]
            .playback
            .as_ref()
            .unwrap()
            .state,
        "playing"
    );
    runtime.user_pause(0, false, &mut engine).unwrap();
    assert!(engine.key_held(keyboards[1], 0x41));
    assert!(runtime.studio_busy(1));
}
