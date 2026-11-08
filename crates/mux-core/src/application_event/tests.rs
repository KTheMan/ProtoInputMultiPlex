use super::*;
use crate::desktop::{Bounds, CursorShape, MouseButton};
use std::collections::BTreeSet;

fn screens() -> ScreenState {
    let desktop = Bounds {
        left: -100,
        top: -50,
        right: 1900,
        bottom: 950,
    };
    ScreenState {
        revision: 1,
        desktop,
        screens: vec![desktop],
    }
}

fn engine() -> Engine {
    Engine::new(71, screens()).unwrap()
}

fn all(pointer_events: bool, key_events: bool) -> Grant {
    Grant {
        users: UserScope::AllOwned,
        pointer_events,
        key_events,
        owned_windows: false,
    }
}

fn only(users: &[UserId], pointer_events: bool, key_events: bool) -> Grant {
    Grant {
        users: UserScope::Only(users.iter().copied().collect()),
        pointer_events,
        key_events,
        owned_windows: false,
    }
}

fn events(
    engine: &mut Engine,
    recipient: RecipientId,
    subscription: SubscriptionId,
) -> Vec<Envelope> {
    match engine.poll(recipient, subscription, MAX_QUEUE).unwrap() {
        Poll::Events { events } => events,
        other => panic!("expected events, got {other:?}"),
    }
}

#[test]
fn two_users_directly_share_one_cooperative_consumer_without_any_target() {
    let mut engine = engine();
    let app = engine.register_recipient("native test consumer").unwrap();
    engine.approve_grant(app, all(true, true)).unwrap();
    let (subscription, snapshot) = engine.subscribe(app, 64).unwrap();
    assert!(snapshot.users.is_empty());
    assert_eq!(snapshot.mode, ApplicationMode::Cooperative);
    let a = engine.create_user(app, "Blue").unwrap();
    let b = engine.create_user(app, "Orange").unwrap();
    engine
        .cursor_control(app, a, CursorControl::Color([30, 120, 255, 255]))
        .unwrap();
    engine
        .cursor_control(app, b, CursorControl::Color([255, 130, 30, 255]))
        .unwrap();
    for (user, position) in [(a, Point { x: 30, y: 40 }), (b, Point { x: 300, y: 400 })] {
        engine
            .submit(app, user, Input::Motion { position })
            .unwrap();
        for button in [
            MouseButton::Primary,
            MouseButton::Secondary,
            MouseButton::Middle,
            MouseButton::X1,
            MouseButton::X2,
        ] {
            let sequence = engine.click(app, user, button).unwrap();
            assert_eq!(sequence[1], sequence[0] + 1);
        }
        for axis in [WheelAxis::Vertical, WheelAxis::Horizontal] {
            engine
                .submit(app, user, Input::Wheel { axis, units: -120 })
                .unwrap();
        }
        engine
            .submit(
                app,
                user,
                Input::Key {
                    key: KeyCode(4),
                    transition: Transition::Down,
                },
            )
            .unwrap();
        engine
            .submit(
                app,
                user,
                Input::Key {
                    key: KeyCode(4),
                    transition: Transition::Up,
                },
            )
            .unwrap();
    }
    let received = events(&mut engine, app, subscription);
    assert_eq!(received.len(), 34);
    assert!(
        received
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(matches!(&received[0].event, Event::UserCreated { user } if user.id == a));
    assert!(matches!(&received[1].event, Event::UserCreated { user } if user.id == b));
    let snapshot = engine.snapshot(app).unwrap();
    assert_ne!(
        snapshot.users[0].cursor.as_ref().unwrap().color,
        snapshot.users[1].cursor.as_ref().unwrap().color
    );
    assert_eq!(
        snapshot.users[0].pointer.as_ref().unwrap().position,
        Point { x: 30, y: 40 }
    );
    assert_eq!(
        snapshot.users[1].pointer.as_ref().unwrap().position,
        Point { x: 300, y: 400 }
    );
    assert!(
        snapshot
            .users
            .iter()
            .all(|user| user.held_keys.as_ref().unwrap().is_empty()
                && user.pointer.as_ref().unwrap().held_buttons.is_empty())
    );
    assert!(
        snapshot
            .capabilities
            .iter()
            .any(|status| status.capability == Capability::TargetInjection && !status.available)
    );
    engine.dispose_user(app, a).unwrap();
    assert!(
        matches!(events(&mut engine, app, subscription)[0].event, Event::UserDisposed { user } if user == a)
    );
    assert_eq!(engine.snapshot(app).unwrap().users.len(), 1);
}

#[test]
fn consent_is_off_and_pointer_key_state_is_redacted_in_snapshot_and_updates() {
    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    let user = engine.create_user(owner, "User").unwrap();
    assert_eq!(engine.subscribe(owner, 4).unwrap_err(), Error::Denied);
    assert_eq!(
        engine
            .submit(
                owner,
                user,
                Input::Motion {
                    position: Point { x: 0, y: 0 }
                }
            )
            .unwrap_err(),
        Error::Denied
    );
    engine.approve_grant(owner, all(true, true)).unwrap();
    engine
        .submit(
            owner,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
    engine
        .submit(
            owner,
            user,
            Input::Button {
                button: MouseButton::X2,
                transition: Transition::Down,
            },
        )
        .unwrap();
    let pointer = engine.register_recipient("pointer observer").unwrap();
    let keyboard = engine.register_recipient("keyboard observer").unwrap();
    engine
        .approve_grant(pointer, only(&[user], true, false))
        .unwrap();
    engine
        .approve_grant(keyboard, only(&[user], false, true))
        .unwrap();
    let (pointer_sub, pointer_snapshot) = engine.subscribe(pointer, 8).unwrap();
    let (key_sub, key_snapshot) = engine.subscribe(keyboard, 8).unwrap();
    assert!(pointer_snapshot.users[0].held_keys.is_none());
    assert_eq!(
        pointer_snapshot.users[0]
            .pointer
            .as_ref()
            .unwrap()
            .held_buttons,
        vec![MouseButton::X2]
    );
    assert!(key_snapshot.users[0].pointer.is_none());
    assert!(key_snapshot.users[0].cursor.is_none());
    assert_eq!(key_snapshot.users[0].held_keys, Some(vec![KeyCode(4)]));
    engine.rename_user(owner, user, "Renamed").unwrap();
    let pointer_updates = events(&mut engine, pointer, pointer_sub);
    let key_updates = events(&mut engine, keyboard, key_sub);
    assert!(
        matches!(&pointer_updates[0].event, Event::UserUpdated { user } if user.held_keys.is_none())
    );
    assert!(
        matches!(&key_updates[0].event, Event::UserUpdated { user } if user.pointer.is_none() && user.cursor.is_none())
    );
    engine
        .submit(
            owner,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Up,
            },
        )
        .unwrap();
    assert!(events(&mut engine, pointer, pointer_sub).is_empty());
    let received = events(&mut engine, keyboard, key_sub);
    assert!(matches!(
        received[0].event,
        Event::Input {
            position: None,
            input: Input::Key { .. },
            ..
        }
    ));
    assert_eq!(
        engine
            .submit(
                pointer,
                user,
                Input::Wheel {
                    axis: WheelAxis::Horizontal,
                    units: 120
                }
            )
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn explicit_user_scope_filters_all_lifecycle_and_input_and_cannot_be_self_changed() {
    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    engine.approve_grant(owner, all(true, true)).unwrap();
    let a = engine.create_user(owner, "A").unwrap();
    let b = engine.create_user(owner, "B").unwrap();
    let observer = engine.register_recipient("observer").unwrap();
    let mut grant = all(true, false);
    grant.users = UserScope::Only(BTreeSet::from([a]));
    engine.approve_grant(observer, grant).unwrap();
    let (sub, snapshot) = engine.subscribe(observer, 8).unwrap();
    assert_eq!(
        snapshot
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![a]
    );
    engine.rename_user(owner, b, "Hidden").unwrap();
    engine
        .submit(
            owner,
            b,
            Input::Motion {
                position: Point { x: 1, y: 2 },
            },
        )
        .unwrap();
    engine.dispose_user(owner, b).unwrap();
    assert!(events(&mut engine, observer, sub).is_empty());
    assert_eq!(engine.poll(owner, sub, 1).unwrap_err(), Error::Denied);
    engine.rename_user(owner, a, "Visible").unwrap();
    assert_eq!(events(&mut engine, observer, sub).len(), 1);
}

#[test]
fn overflow_has_explicit_gap_and_resubscribe_always_starts_with_fresh_snapshot() {
    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    engine.approve_grant(owner, all(true, false)).unwrap();
    let user = engine.create_user(owner, "User").unwrap();
    let (slow, initial) = engine.subscribe(owner, 1).unwrap();
    let (fast, _) = engine.subscribe(owner, 8).unwrap();
    for x in 0..3 {
        engine
            .submit(
                owner,
                user,
                Input::Motion {
                    position: Point { x, y: 0 },
                },
            )
            .unwrap();
    }
    let stats = engine.subscriber_stats(owner, slow).unwrap();
    assert_eq!(stats.queued, 0);
    assert!(stats.overflowed);
    assert!(
        matches!(engine.poll(owner, slow, 1).unwrap(), Poll::Gap { after_sequence, lost_through } if after_sequence == initial.through_sequence && lost_through == initial.through_sequence + 3)
    );
    assert_eq!(events(&mut engine, owner, fast).len(), 3);
    let (fresh, snapshot) = engine.resubscribe(owner, slow, 2).unwrap();
    assert_eq!(snapshot.users[0].pointer.as_ref().unwrap().position.x, 2);
    assert_eq!(
        engine.poll(owner, slow, 1).unwrap_err(),
        Error::MissingSubscription
    );
    assert!(events(&mut engine, owner, fresh).is_empty());
    engine
        .submit(
            owner,
            user,
            Input::Motion {
                position: Point { x: 4, y: 0 },
            },
        )
        .unwrap();
    assert_eq!(
        events(&mut engine, owner, fresh)[0].sequence,
        snapshot.through_sequence + 1
    );
}

#[test]
fn revoke_purges_queued_keys_before_poll_and_regrant_does_not_replay() {
    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    engine.approve_grant(owner, all(true, true)).unwrap();
    let user = engine.create_user(owner, "User").unwrap();
    let (sub, _) = engine.subscribe(owner, 8).unwrap();
    engine
        .submit(
            owner,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
    engine.approve_grant(owner, all(true, false)).unwrap();
    assert_eq!(engine.subscriber_stats(owner, sub).unwrap().queued, 0);
    assert_eq!(
        engine.poll(owner, sub, 8).unwrap(),
        Poll::Ended {
            reason: EndReason::GrantChanged
        }
    );
    let (fresh, snapshot) = engine.subscribe(owner, 8).unwrap();
    assert!(snapshot.users[0].held_keys.is_none());
    assert!(events(&mut engine, owner, fresh).is_empty());
    engine.approve_grant(owner, Grant::default()).unwrap();
    assert_eq!(
        engine.poll(owner, fresh, 8).unwrap(),
        Poll::Ended {
            reason: EndReason::Revoked
        }
    );
    assert_eq!(engine.snapshot(owner).unwrap_err(), Error::Denied);
}

#[test]
fn disconnect_disposal_reset_and_end_retire_held_state_without_stale_replay() {
    let mut engine = engine();
    let first = engine.register_recipient("first").unwrap();
    let second = engine.register_recipient("second").unwrap();
    engine.approve_grant(first, all(true, true)).unwrap();
    engine.approve_grant(second, all(true, true)).unwrap();
    let a = engine.create_user(first, "A").unwrap();
    let b = engine.create_user(second, "B").unwrap();
    engine
        .approve_grant(second, only(&[a, b], true, true))
        .unwrap();
    let (first_sub, _) = engine.subscribe(first, 8).unwrap();
    let (second_sub, _) = engine.subscribe(second, 8).unwrap();
    engine
        .submit(
            first,
            a,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
    engine.disconnect_recipient(first).unwrap();
    assert_eq!(
        engine.poll(first, first_sub, 8).unwrap(),
        Poll::Ended {
            reason: EndReason::RecipientDisconnected
        }
    );
    assert_eq!(
        engine
            .snapshot(second)
            .unwrap()
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![b]
    );
    assert!(
        events(&mut engine, second, second_sub)
            .iter()
            .any(|event| matches!(event.event, Event::UserDisposed { user } if user == a))
    );
    engine
        .submit(
            second,
            b,
            Input::Motion {
                position: Point { x: 0, y: 0 },
            },
        )
        .unwrap();
    engine.reset(72, screens()).unwrap();
    assert_eq!(
        engine.poll(second, second_sub, 8).unwrap(),
        Poll::Ended {
            reason: EndReason::Reset
        }
    );
    assert_eq!(
        engine.snapshot(second).unwrap_err(),
        Error::MissingRecipient
    );
    let next = engine.register_recipient("new epoch").unwrap();
    engine.approve_grant(next, all(true, true)).unwrap();
    let new_user = engine.create_user(next, "New").unwrap();
    assert_ne!(new_user, b);
    let (new_sub, snapshot) = engine.subscribe(next, 8).unwrap();
    assert_eq!(snapshot.epoch, 72);
    assert_eq!(snapshot.users.len(), 1);
    assert!(snapshot.users[0].held_keys.as_ref().unwrap().is_empty());
    assert!(events(&mut engine, next, new_sub).is_empty());
    engine.end();
    assert_eq!(
        engine.poll(next, new_sub, 8).unwrap(),
        Poll::Ended {
            reason: EndReason::SessionEnded
        }
    );
    assert_eq!(engine.subscribe(next, 8).unwrap_err(), Error::Ended);
    assert_eq!(
        engine
            .submit(
                next,
                new_user,
                Input::Motion {
                    position: Point { x: 0, y: 0 }
                }
            )
            .unwrap_err(),
        Error::Ended
    );
}

#[test]
fn cursor_controls_roundtrip_and_unsupported_or_locked_changes_are_atomic() {
    let mut engine = engine();
    let app = engine.register_recipient("app").unwrap();
    engine.approve_grant(app, all(true, false)).unwrap();
    let user = engine.create_user(app, "User").unwrap();
    engine
        .cursor_control(app, user, CursorControl::Visible(false))
        .unwrap();
    engine
        .cursor_control(app, user, CursorControl::Size(80))
        .unwrap();
    engine
        .cursor_control(app, user, CursorControl::Color([255, 100, 30, 255]))
        .unwrap();
    engine
        .cursor_control(app, user, CursorControl::Shape(CursorShape::Ring))
        .unwrap();
    engine
        .cursor_control(app, user, CursorControl::TypeLocked(true))
        .unwrap();
    engine
        .cursor_control(app, user, CursorControl::MovementLocked(true))
        .unwrap();
    let before = engine.snapshot(app).unwrap();
    assert_eq!(
        engine
            .cursor_control(app, user, CursorControl::Shape(CursorShape::Arrow))
            .unwrap_err(),
        Error::Locked
    );
    assert!(matches!(
        engine.cursor_control(app, user, CursorControl::Shape(CursorShape::System)),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        engine.cursor_control(app, user, CursorControl::Theme("theme".into())),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        engine.cursor_control(app, user, CursorControl::Flags(1)),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        engine.cursor_control(app, user, CursorControl::Size(1)),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        engine
            .submit(
                app,
                user,
                Input::Motion {
                    position: Point { x: 10, y: 20 }
                }
            )
            .unwrap_err(),
        Error::Locked
    );
    assert_eq!(engine.snapshot(app).unwrap(), before);
    assert!(matches!(
        engine.set_mode(ApplicationMode::TargetInjection),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        engine.host_control(HostControl::Hide),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn invalid_transitions_and_coordinates_never_publish_or_mutate_state() {
    let mut engine = engine();
    let app = engine.register_recipient("app").unwrap();
    engine.approve_grant(app, all(true, true)).unwrap();
    let user = engine.create_user(app, "User").unwrap();
    let before = engine.snapshot(app).unwrap();
    for input in [
        Input::Button {
            button: MouseButton::Primary,
            transition: Transition::Up,
        },
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Up,
        },
        Input::Key {
            key: KeyCode(0),
            transition: Transition::Down,
        },
        Input::Wheel {
            axis: WheelAxis::Vertical,
            units: 0,
        },
        Input::Motion {
            position: Point { x: 1900, y: 0 },
        },
    ] {
        assert!(engine.submit(app, user, input).is_err());
    }
    assert_eq!(engine.snapshot(app).unwrap(), before);
    engine
        .submit(
            app,
            user,
            Input::Button {
                button: MouseButton::Primary,
                transition: Transition::Down,
            },
        )
        .unwrap();
    let held = engine.snapshot(app).unwrap();
    assert_eq!(
        engine.click(app, user, MouseButton::Primary).unwrap_err(),
        Error::InvalidTransition
    );
    assert_eq!(engine.snapshot(app).unwrap(), held);
}

#[test]
fn canvas_conversion_preserves_negative_desktop_origin_and_revision() {
    let transform = CanvasTransform {
        revision: 7,
        screen: screens().desktop,
        width: 1000.0,
        height: 500.0,
    };
    let screen = Point { x: 300, y: 250 };
    let canvas = transform.to_canvas(screen, 7).unwrap();
    assert_eq!(canvas, [200.0, 150.0]);
    assert_eq!(transform.to_screen(canvas, 7).unwrap(), screen);
    assert_eq!(
        transform.to_canvas(screen, 6).unwrap_err(),
        Error::StaleRevision
    );
    assert!(transform.to_screen([f64::NAN, 1.0], 7).is_err());
}

struct TestOwnWindowAdapter;
impl OwnWindowAdapter for TestOwnWindowAdapter {
    fn supported(&self, _window: WindowId, action: WindowAction) -> bool {
        action == WindowAction::Hide
    }
    fn apply(
        &mut self,
        _owner: RecipientId,
        current: &WindowInfo,
        _control: &WindowControl,
    ) -> Result<WindowInfo, Error> {
        // Test fake explicitly stands in for backend observation, not native acceptance.
        let mut observed = current.clone();
        observed.visible = false;
        Ok(observed)
    }
}

#[test]
fn windows_are_owned_tracked_and_backend_acknowledged_with_explicit_unavailability() {
    let mut engine = engine();
    let app = engine.register_recipient("app").unwrap();
    let other = engine.register_recipient("other").unwrap();
    let mut grant = all(false, false);
    grant.owned_windows = true;
    engine.approve_grant(app, grant.clone()).unwrap();
    engine.approve_grant(other, grant).unwrap();
    let window = engine
        .register_owned_window(
            app,
            WindowObservation {
                bounds: screens().desktop,
                metadata: WindowMetadata {
                    title: Some("Owned window".into()),
                    process_id: Some(42),
                    ui_thread_id: Some(7),
                    client_bounds: Some(Bounds {
                        left: -390,
                        top: 20,
                        right: 390,
                        bottom: 570,
                    }),
                    dpi: Some(144),
                    monitor_bounds: Some(screens().desktop),
                    monitor_work_area: Some(screens().desktop),
                },
                visible: true,
                minimized: false,
                maximized: false,
                click_through: false,
                enabled: true,
                topmost: false,
                opacity: 255,
            },
        )
        .unwrap();
    let (sub, _) = engine.subscribe(app, 8).unwrap();
    let (other_sub, _) = engine.subscribe(other, 8).unwrap();
    assert_eq!(
        engine.window_info(other, window).unwrap_err(),
        Error::Denied
    );
    assert!(engine.snapshot(other).unwrap().windows.is_empty());
    engine.track_window(app, window).unwrap();
    assert_eq!(engine.snapshot(app).unwrap().windows.len(), 1);
    assert_eq!(
        engine
            .window_info(app, window)
            .unwrap()
            .metadata
            .title
            .as_deref(),
        Some("Owned window")
    );
    assert!(matches!(
        engine.control_window(
            app,
            window,
            WindowControl::Hide,
            &mut UnavailableWindowAdapter
        ),
        Err(Error::Unsupported(_))
    ));
    assert!(engine.window_info(app, window).unwrap().visible);
    let ack = engine
        .control_window(app, window, WindowControl::Hide, &mut TestOwnWindowAdapter)
        .unwrap();
    assert!(!ack.observed.visible);
    assert!(matches!(
        engine.control_window(app, window, WindowControl::Focus, &mut TestOwnWindowAdapter),
        Err(Error::Unsupported(_))
    ));
    engine.untrack_window(app, window).unwrap();
    assert!(engine.snapshot(app).unwrap().windows.is_empty());
    let received = events(&mut engine, app, sub);
    assert_eq!(received.len(), 3);
    assert!(events(&mut engine, other, other_sub).is_empty());
    assert!(matches!(
        &received[0].event,
        Event::Window {
            update: WindowUpdate::Tracked { window }
        } if window.metadata.dpi == Some(144)
    ));
    assert!(matches!(
        received[0].event,
        Event::Window {
            update: WindowUpdate::Tracked { .. }
        }
    ));
    assert!(matches!(
        received[1].event,
        Event::Window {
            update: WindowUpdate::Changed { .. }
        }
    ));
    assert!(matches!(
        received[2].event,
        Event::Window {
            update: WindowUpdate::Untracked { .. }
        }
    ));
    engine.track_window(app, window).unwrap();
    engine.destroy_window(window).unwrap();
    assert!(events(&mut engine, app, sub).iter().any(|event| matches!(
        event.event,
        Event::Window {
            update: WindowUpdate::Destroyed { .. }
        }
    )));
    assert_eq!(
        engine.window_info(app, window).unwrap_err(),
        Error::MissingWindow
    );
}

#[test]
fn window_metadata_defaults_on_old_wire_snapshots_and_bounds_native_facts() {
    let base = WindowObservation {
        bounds: screens().desktop,
        metadata: WindowMetadata::default(),
        visible: true,
        minimized: false,
        maximized: false,
        click_through: false,
        enabled: true,
        topmost: false,
        opacity: 255,
    };
    let mut old_observation = serde_json::to_value(&base).unwrap();
    old_observation.as_object_mut().unwrap().remove("metadata");
    assert_eq!(
        serde_json::from_value::<WindowObservation>(old_observation)
            .unwrap()
            .metadata,
        WindowMetadata::default()
    );

    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    let id = engine.register_owned_window(owner, base.clone()).unwrap();
    let original = engine.window_info(owner, id).unwrap_err();
    assert_eq!(original, Error::Denied);
    let mut grant = all(false, false);
    grant.owned_windows = true;
    engine.approve_grant(owner, grant).unwrap();
    let original = engine.window_info(owner, id).unwrap();
    let mut old_info = serde_json::to_value(&original).unwrap();
    old_info.as_object_mut().unwrap().remove("metadata");
    assert_eq!(
        serde_json::from_value::<WindowInfo>(old_info)
            .unwrap()
            .metadata,
        WindowMetadata::default()
    );

    let cases = [
        (
            WindowMetadata {
                title: Some("é".repeat(257)),
                ..WindowMetadata::default()
            },
            Error::Invalid("window title"),
        ),
        (
            WindowMetadata {
                title: Some("nul\0title".into()),
                ..WindowMetadata::default()
            },
            Error::Invalid("window title"),
        ),
        (
            WindowMetadata {
                process_id: Some(0),
                ..WindowMetadata::default()
            },
            Error::Invalid("window owner identity"),
        ),
        (
            WindowMetadata {
                client_bounds: Some(Bounds {
                    left: 10,
                    top: 0,
                    right: 9,
                    bottom: 10,
                }),
                ..WindowMetadata::default()
            },
            Error::Invalid("window client bounds"),
        ),
        (
            WindowMetadata {
                client_bounds: Some(Bounds {
                    left: 0,
                    top: 0,
                    right: 1_000_001,
                    bottom: 10,
                }),
                ..WindowMetadata::default()
            },
            Error::Invalid("window client bounds"),
        ),
        (
            WindowMetadata {
                dpi: Some(0),
                ..WindowMetadata::default()
            },
            Error::Invalid("window dpi"),
        ),
        (
            WindowMetadata {
                dpi: Some(4097),
                ..WindowMetadata::default()
            },
            Error::Invalid("window dpi"),
        ),
        (
            WindowMetadata {
                monitor_work_area: Some(screens().desktop),
                ..WindowMetadata::default()
            },
            Error::Invalid("window monitor work area"),
        ),
        (
            WindowMetadata {
                monitor_bounds: Some(screens().desktop),
                monitor_work_area: Some(Bounds {
                    left: -101,
                    ..screens().desktop
                }),
                ..WindowMetadata::default()
            },
            Error::Invalid("window monitor work area"),
        ),
    ];

    for (metadata, error) in cases {
        let mut observation = base.clone();
        observation.metadata = metadata.clone();
        assert_eq!(
            engine
                .register_owned_window(owner, observation)
                .unwrap_err(),
            error
        );
        let mut changed = original.clone();
        changed.metadata = metadata;
        assert_eq!(
            engine.update_owned_window(owner, changed).unwrap_err(),
            error
        );
        assert_eq!(engine.window_info(owner, id).unwrap(), original);
    }

    let mut zero_client = original.clone();
    zero_client.metadata.client_bounds = Some(Bounds {
        left: -10,
        top: 20,
        right: -10,
        bottom: 20,
    });
    engine
        .update_owned_window(owner, zero_client.clone())
        .unwrap();
    assert_eq!(engine.window_info(owner, id).unwrap(), zero_client);
}

#[test]
fn producer_revocation_releases_held_state_for_other_observers_and_regrant() {
    let mut engine = engine();
    let owner = engine.register_recipient("owner").unwrap();
    let observer = engine.register_recipient("observer").unwrap();
    engine.approve_grant(owner, all(true, true)).unwrap();
    let user = engine.create_user(owner, "User").unwrap();
    engine
        .approve_grant(observer, only(&[user], true, true))
        .unwrap();
    engine
        .submit(
            owner,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
    engine
        .submit(
            owner,
            user,
            Input::Button {
                button: MouseButton::Primary,
                transition: Transition::Down,
            },
        )
        .unwrap();
    let (sub, _) = engine.subscribe(observer, 8).unwrap();
    engine.approve_grant(owner, Grant::default()).unwrap();
    let received = events(&mut engine, observer, sub);
    assert!(
        matches!(&received[0].event, Event::UserUpdated { user } if user.held_keys.as_ref().unwrap().is_empty() && user.pointer.as_ref().unwrap().held_buttons.is_empty())
    );
    engine.approve_grant(owner, all(true, true)).unwrap();
    let snapshot = engine.snapshot(owner).unwrap();
    assert!(snapshot.users[0].held_keys.as_ref().unwrap().is_empty());
    assert!(
        snapshot.users[0]
            .pointer
            .as_ref()
            .unwrap()
            .held_buttons
            .is_empty()
    );
}

#[test]
fn all_owned_isolates_two_recipients_across_snapshot_lifecycle_input_and_revoke() {
    let mut engine = engine();
    let a = engine.register_recipient("consumer A").unwrap();
    let b = engine.register_recipient("consumer B").unwrap();
    engine.approve_grant(a, all(true, true)).unwrap();
    engine.approve_grant(b, all(true, true)).unwrap();
    let a_user = engine.create_user(a, "A user").unwrap();
    let b_user = engine.create_user(b, "B user").unwrap();
    let (a_sub, a_snapshot) = engine.subscribe(a, 32).unwrap();
    let (b_sub, b_snapshot) = engine.subscribe(b, 32).unwrap();
    assert_eq!(
        a_snapshot
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![a_user]
    );
    assert_eq!(
        b_snapshot
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![b_user]
    );

    engine.rename_user(a, a_user, "A renamed").unwrap();
    engine
        .cursor_control(a, a_user, CursorControl::Color([1, 2, 3, 255]))
        .unwrap();
    engine
        .submit(
            a,
            a_user,
            Input::Motion {
                position: Point { x: 10, y: 20 },
            },
        )
        .unwrap();
    engine
        .submit(
            a,
            a_user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
    assert_eq!(events(&mut engine, a, a_sub).len(), 4);
    assert!(events(&mut engine, b, b_sub).is_empty());
    engine.rename_user(b, b_user, "B renamed").unwrap();
    engine
        .cursor_control(b, b_user, CursorControl::Color([4, 5, 6, 255]))
        .unwrap();
    engine
        .submit(
            b,
            b_user,
            Input::Motion {
                position: Point { x: 100, y: 200 },
            },
        )
        .unwrap();
    engine
        .submit(
            b,
            b_user,
            Input::Key {
                key: KeyCode(5),
                transition: Transition::Down,
            },
        )
        .unwrap();
    assert_eq!(events(&mut engine, b, b_sub).len(), 4);
    assert!(events(&mut engine, a, a_sub).is_empty());

    let a_late = engine.create_user(a, "A late").unwrap();
    let b_late = engine.create_user(b, "B late").unwrap();
    let a_events = events(&mut engine, a, a_sub);
    let b_events = events(&mut engine, b, b_sub);
    assert_eq!(a_events.len(), 1);
    assert_eq!(b_events.len(), 1);
    assert!(
        matches!(&a_events[0].event, Event::UserCreated { user } if user.id == a_late && user.owned_by_recipient)
    );
    assert!(
        matches!(&b_events[0].event, Event::UserCreated { user } if user.id == b_late && user.owned_by_recipient)
    );

    engine.approve_grant(b, all(true, false)).unwrap();
    assert!(events(&mut engine, a, a_sub).is_empty());
    assert_eq!(
        engine.poll(b, b_sub, 32).unwrap(),
        Poll::Ended {
            reason: EndReason::GrantChanged
        }
    );
    let (b_sub, b_snapshot) = engine.subscribe(b, 32).unwrap();
    assert_eq!(
        b_snapshot
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![b_user, b_late]
    );
    assert!(
        b_snapshot
            .users
            .iter()
            .all(|user| user.held_keys.is_none() && user.owned_by_recipient)
    );

    // A cross-owner observer works only after the host explicitly names A's user.
    let observer = engine.register_recipient("explicit observer").unwrap();
    engine
        .approve_grant(observer, only(&[a_user], true, true))
        .unwrap();
    let (observer_sub, observed) = engine.subscribe(observer, 8).unwrap();
    assert_eq!(
        observed
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![a_user]
    );
    assert_eq!(observed.users[0].held_keys, Some(vec![KeyCode(4)]));
    assert!(!observed.users[0].owned_by_recipient);
    engine.rename_user(a, a_user, "Explicitly visible").unwrap();
    let updates = events(&mut engine, observer, observer_sub);
    assert_eq!(updates.len(), 1);
    assert!(matches!(&updates[0].event, Event::UserUpdated { user } if !user.owned_by_recipient));
    events(&mut engine, a, a_sub);
    assert!(events(&mut engine, b, b_sub).is_empty());

    engine.dispose_user(a, a_user).unwrap();
    assert!(
        matches!(events(&mut engine, a, a_sub)[0].event, Event::UserDisposed { user } if user == a_user)
    );
    assert!(
        matches!(events(&mut engine, observer, observer_sub)[0].event, Event::UserDisposed { user } if user == a_user)
    );
    assert!(events(&mut engine, b, b_sub).is_empty());
    engine.dispose_user(b, b_late).unwrap();
    assert!(
        matches!(events(&mut engine, b, b_sub)[0].event, Event::UserDisposed { user } if user == b_late)
    );
    assert!(events(&mut engine, a, a_sub).is_empty());
    assert!(events(&mut engine, observer, observer_sub).is_empty());
}
