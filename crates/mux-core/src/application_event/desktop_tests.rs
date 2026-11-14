use super::*;
use crate::desktop::{Bounds, CursorShape, MouseButton};
use std::collections::BTreeSet;

fn screens() -> ScreenState {
    let desktop = Bounds {
        left: -100,
        top: -50,
        right: 900,
        bottom: 750,
    };
    ScreenState {
        revision: 1,
        desktop,
        screens: vec![desktop],
    }
}
fn key() -> WindowsVirtualKey {
    WindowsVirtualKey {
        virtual_key: 0xA3,
        scan_code: 0x1D,
        prefix: ScanPrefix::E0,
    }
}
fn baseline() -> DesktopBaselineV1 {
    DesktopBaselineV1 {
        version: 1,
        next_sequence: 1,
        screens: screens(),
        position: Point { x: 7, y: 8 },
        cursor: ConfiguredDesktopCursor {
            shape: CursorShape::System,
            size: 37.5,
            color: [1, 2, 3],
            visible: true,
            follow_application: true,
        },
        held_buttons: vec![MouseButton::Primary],
        native_held_keys: vec![key()],
    }
}
fn grant(user: UserId, pointer_events: bool, key_events: bool) -> Grant {
    Grant {
        users: UserScope::Only(BTreeSet::from([user])),
        pointer_events,
        key_events,
        owned_windows: false,
        host_state: false,
    }
}
fn approve(engine: &mut Engine, recipient: RecipientId, text_units: bool) {
    engine
        .approve_desktop_observation(
            recipient,
            Some(DesktopConsentV1 {
                version: 1,
                text_units,
            }),
        )
        .unwrap();
    engine
        .negotiate_desktop_observation(recipient, Some(1))
        .unwrap();
}
fn record(sequence: u64, change: DesktopObservation) -> DesktopRecordV1 {
    DesktopRecordV1 {
        version: 1,
        sequence,
        screen_revision: 1,
        admission: DesktopAdmission::SharedNativeHold,
        change,
    }
}

#[test]
fn desktop_baseline_requires_both_consent_and_decoder_and_exact_scope() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("private provider").unwrap();
    let recipient = engine.register_recipient("receiver").unwrap();
    let user = engine
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    engine
        .approve_grant(recipient, grant(user, true, true))
        .unwrap();
    assert_eq!(
        engine.negotiate_desktop_observation(recipient, Some(1)),
        Err(Error::Denied)
    );
    assert!(engine.snapshot(recipient).unwrap().users.is_empty());
    engine
        .approve_desktop_observation(
            recipient,
            Some(DesktopConsentV1 {
                version: 1,
                text_units: false,
            }),
        )
        .unwrap();
    let legacy = engine.snapshot(recipient).unwrap();
    assert!(legacy.users.is_empty());
    assert!(
        !legacy
            .capabilities
            .iter()
            .any(|cap| cap.capability == Capability::DesktopObservationV1)
    );
    engine
        .negotiate_desktop_observation(recipient, Some(1))
        .unwrap();
    let view = engine.snapshot(recipient).unwrap().users.remove(0);
    assert!(!view.owned_by_recipient);
    assert!(view.held_keys.is_none());
    assert!(view.cursor.is_none());
    assert_eq!(view.pointer.unwrap().held_buttons, [MouseButton::Primary]);
    let observed = view.desktop.unwrap();
    assert_eq!(observed.last_sequence, 0);
    assert_eq!(observed.native_held_keys.unwrap(), [key()]);
    assert_eq!(observed.configured_cursor.unwrap(), baseline().cursor);
    assert!(!observed.text_units);
    engine
        .approve_grant(
            owner,
            Grant {
                users: UserScope::AllOwned,
                pointer_events: true,
                key_events: true,
                ..Default::default()
            },
        )
        .unwrap();
    approve(&mut engine, owner, true);
    assert!(engine.snapshot(owner).unwrap().users.is_empty());
}

#[test]
fn desktop_family_redaction_preserves_native_identity_and_utf16_units() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("private provider").unwrap();
    let pointer = engine.register_recipient("pointer receiver").unwrap();
    let keys = engine.register_recipient("key receiver").unwrap();
    let text = engine.register_recipient("text receiver").unwrap();
    let user = engine
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    let mut subscriptions = Vec::new();
    for (recipient, pointer_events, key_events, text_units) in [
        (pointer, true, false, false),
        (keys, false, true, false),
        (text, false, true, true),
    ] {
        engine
            .approve_grant(recipient, grant(user, pointer_events, key_events))
            .unwrap();
        approve(&mut engine, recipient, text_units);
        let (subscription, snapshot) = engine.subscribe(recipient, 16).unwrap();
        let view = &snapshot.users[0];
        assert_eq!(view.pointer.is_some(), pointer_events);
        assert_eq!(
            view.desktop.as_ref().unwrap().configured_cursor.is_some(),
            pointer_events
        );
        assert_eq!(
            view.desktop.as_ref().unwrap().native_held_keys.is_some(),
            key_events
        );
        subscriptions.push((recipient, subscription));
    }
    for item in [
        record(
            1,
            DesktopObservation::Motion {
                position: Point { x: 9, y: 10 },
            },
        ),
        record(
            2,
            DesktopObservation::Key {
                key: key(),
                transition: Transition::Up,
            },
        ),
        record(3, DesktopObservation::TextUnit { unit: 0xD83D }),
        record(4, DesktopObservation::TextUnit { unit: 0xDE00 }),
    ] {
        engine
            .publish_desktop_observation(owner, user, item)
            .unwrap();
    }
    for (recipient, subscription) in subscriptions {
        let Poll::Events { events } = engine.poll(recipient, subscription, 16).unwrap() else {
            panic!("expected records")
        };
        let records: Vec<_> = events
            .into_iter()
            .map(|envelope| match envelope.event {
                Event::DesktopObserved {
                    record, position, ..
                } => {
                    assert_eq!(position.is_some(), recipient == pointer);
                    record
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        let sequences: Vec<_> = records.iter().map(|record| record.sequence).collect();
        assert_eq!(
            sequences,
            if recipient == pointer {
                vec![1]
            } else if recipient == keys {
                vec![2]
            } else {
                vec![2, 3, 4]
            }
        );
        if recipient == text {
            assert_eq!(
                records[1].change,
                DesktopObservation::TextUnit { unit: 0xD83D }
            );
        }
    }
}

#[test]
fn desktop_mutation_is_denied_even_to_private_owner_and_bad_sequence_retires() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("provider").unwrap();
    let receiver = engine.register_recipient("receiver").unwrap();
    let user = engine
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    engine
        .approve_grant(owner, grant(user, true, true))
        .unwrap();
    assert_eq!(
        engine.rename_user(owner, user, "Changed"),
        Err(Error::Denied)
    );
    assert_eq!(engine.dispose_user(owner, user), Err(Error::Denied));
    assert_eq!(
        engine.cursor_control(owner, user, CursorControl::Visible(false)),
        Err(Error::Denied)
    );
    assert_eq!(
        engine.submit(
            owner,
            user,
            Input::Motion {
                position: Point { x: 2, y: 3 }
            }
        ),
        Err(Error::Denied)
    );
    engine
        .approve_grant(receiver, grant(user, true, true))
        .unwrap();
    approve(&mut engine, receiver, true);
    let (subscription, _) = engine.subscribe(receiver, 16).unwrap();
    engine
        .publish_desktop_observation(
            owner,
            user,
            record(1, DesktopObservation::TextUnit { unit: 65 }),
        )
        .unwrap();
    assert_eq!(
        engine.publish_desktop_observation(
            owner,
            user,
            record(3, DesktopObservation::TextUnit { unit: 66 })
        ),
        Err(Error::StaleRevision)
    );
    assert!(!engine.is_desktop_observed(user));
    let Poll::Events { events } = engine.poll(receiver, subscription, 16).unwrap() else {
        panic!("expected disposal")
    };
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, Event::UserDisposed { user: id } if id == user));
}

#[test]
fn desktop_retirement_scrubs_queued_provider_records_without_disposing_other_users() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("provider").unwrap();
    let receiver = engine.register_recipient("receiver").unwrap();
    approve(&mut engine, receiver, true);
    // Select IDs after creation, then subscribe: one known provider baseline and one cooperative user.
    let observed = engine
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    let cooperative = engine.create_user(owner, "Cooperative").unwrap();
    engine
        .approve_grant(
            receiver,
            Grant {
                users: UserScope::Only(BTreeSet::from([observed, cooperative])),
                pointer_events: true,
                key_events: true,
                ..Default::default()
            },
        )
        .unwrap();
    let (subscription, _) = engine.subscribe(receiver, 16).unwrap();
    engine
        .approve_grant(
            owner,
            Grant {
                users: UserScope::AllOwned,
                pointer_events: true,
                key_events: true,
                ..Default::default()
            },
        )
        .unwrap();
    engine
        .submit(
            owner,
            cooperative,
            Input::Motion {
                position: Point { x: 20, y: 30 },
            },
        )
        .unwrap();
    engine
        .publish_desktop_observation(
            owner,
            observed,
            record(1, DesktopObservation::TextUnit { unit: 1 }),
        )
        .unwrap();
    engine.retire_desktop_observed(owner, observed).unwrap();
    assert_eq!(engine.snapshot(receiver).unwrap().users[0].id, cooperative);
    let Poll::Events { events } = engine.poll(receiver, subscription, 16).unwrap() else {
        panic!("expected disposal")
    };
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.event, Event::DesktopObserved { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.event, Event::Input { user, .. } if user == cooperative))
    );
    // Private provider disconnect also retires its mappings, never leaves an orphan owner.
    let another = engine
        .create_desktop_observed(owner, "Another", baseline())
        .unwrap();
    engine.disconnect_recipient(owner).unwrap();
    assert!(!engine.is_desktop_observed(another));
}

#[test]
fn desktop_baseline_rejects_ambiguous_identity_and_screen_mismatch() {
    let mut duplicate = baseline();
    duplicate.native_held_keys.push(WindowsVirtualKey {
        prefix: ScanPrefix::None,
        ..key()
    });
    assert!(duplicate.validate().is_err());
    let mut invalid = baseline();
    invalid.cursor.size = f32::NAN;
    assert!(invalid.validate().is_err());
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("provider").unwrap();
    let mut mismatch = baseline();
    mismatch.screens.revision = 2;
    assert_eq!(
        engine.create_desktop_observed(owner, "Observed", mismatch),
        Err(Error::StaleRevision)
    );
}

#[test]
fn desktop_button_wrapper_preserves_serial_position_before_later_batch_motion() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("provider").unwrap();
    let receiver = engine.register_recipient("pointer receiver").unwrap();
    let user = engine
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    engine
        .approve_grant(receiver, grant(user, true, false))
        .unwrap();
    approve(&mut engine, receiver, false);
    let (subscription, _) = engine.subscribe(receiver, 16).unwrap();
    let click_position = Point { x: 100, y: 110 };
    let final_position = Point { x: 300, y: 310 };
    for item in [
        record(
            1,
            DesktopObservation::Button {
                button: MouseButton::Primary,
                transition: Transition::Up,
            },
        ),
        record(
            2,
            DesktopObservation::Motion {
                position: click_position,
            },
        ),
        record(
            3,
            DesktopObservation::Button {
                button: MouseButton::Primary,
                transition: Transition::Down,
            },
        ),
        record(
            4,
            DesktopObservation::Button {
                button: MouseButton::Primary,
                transition: Transition::Up,
            },
        ),
        record(
            5,
            DesktopObservation::Motion {
                position: final_position,
            },
        ),
    ] {
        engine
            .publish_desktop_observation(owner, user, item)
            .unwrap();
    }
    assert_eq!(
        engine.snapshot(receiver).unwrap().users[0]
            .pointer
            .as_ref()
            .unwrap()
            .position,
        final_position
    );
    let Poll::Events { events } = engine.poll(receiver, subscription, 16).unwrap() else {
        panic!("expected records")
    };
    assert_eq!(events.len(), 5);
    for event in &events[2..4] {
        assert!(
            matches!(event.event, Event::DesktopObserved { position: Some(position), .. } if position == click_position)
        );
    }
}
