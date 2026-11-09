use crate::*;
use mux_core::desktop::Bounds;
use std::collections::BTreeSet;

#[test]
fn remote_window_publication_and_exact_delegation_are_epoch_scoped() {
    let mut host: Host = Host::new(71, screens()).unwrap();
    let publisher = host.register_recipient("native publisher").unwrap();
    let browser = host
        .register_recipient("separate browser receiver")
        .unwrap();
    let window_only = Grant {
        users: UserScope::Only(Default::default()),
        pointer_events: false,
        key_events: false,
        owned_windows: true,
        host_state: false,
    };
    host.approve_events(publisher, window_only.clone()).unwrap();
    host.approve_events(browser, window_only).unwrap();
    host.approve_controls(
        browser,
        ControlGrant {
            window_tracking: true,
            window_control: true,
            ..Default::default()
        },
    )
    .unwrap();
    let observed = WindowObservation {
        bounds: screens().desktop,
        metadata: WindowMetadata {
            title: Some("Owned window".into()),
            ..Default::default()
        },
        visible: true,
        minimized: false,
        maximized: false,
        click_through: false,
        enabled: true,
        topmost: false,
        opacity: 255,
    };
    assert_eq!(
        host.register_remote_window(publisher, observed.clone()),
        Err(Error::Denied)
    );
    host.approve_controls(
        publisher,
        ControlGrant {
            window_publication: true,
            ..Default::default()
        },
    )
    .unwrap();
    let window = host.register_remote_window(publisher, observed).unwrap();
    let reference = RemoteWindowRef { epoch: 71, window };
    assert_eq!(
        host.dispatch(
            browser,
            request(ClientCommand::RemoteWindowQuery { reference })
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    host.delegate_window(
        window,
        browser,
        WindowAccess {
            observe: true,
            control: false,
        },
    )
    .unwrap();
    assert!(matches!(
        host.dispatch(browser, request(ClientCommand::RemoteWindowQuery { reference })),
        Ok(ClientResponse::Window { window: info }) if info.id == window
    ));
    assert!(matches!(
        host.dispatch(browser, request(ClientCommand::RemoteWindowTrack { reference })),
        Ok(ClientResponse::Window { window: info }) if info.id == window
    ));
    assert_eq!(
        host.authorize_remote_window_control(browser, window, &WindowControl::Hide),
        Err(Error::Denied)
    );
    host.delegate_window(
        window,
        browser,
        WindowAccess {
            observe: true,
            control: true,
        },
    )
    .unwrap();
    assert_eq!(
        host.authorize_remote_window_control(browser, window, &WindowControl::Hide),
        Ok(publisher)
    );
    host.approve_controls(
        browser,
        ControlGrant {
            window_control: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        host.authorize_remote_window_control(browser, window, &WindowControl::Hide),
        Err(Error::Denied)
    );
    assert_eq!(
        host.dispatch(
            browser,
            request(ClientCommand::RemoteWindowQuery {
                reference: RemoteWindowRef { epoch: 70, window },
            }),
        )
        .unwrap_err()
        .code,
        FailureCode::StaleRevision
    );
    host.delegate_window(window, browser, WindowAccess::default())
        .unwrap();
    assert_eq!(
        host.dispatch(
            browser,
            request(ClientCommand::RemoteWindowQuery { reference })
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
}

#[test]
fn client_rejects_invalid_window_metadata_before_folding_snapshot() {
    let mut engine = Engine::new(71, screens()).unwrap();
    let owner = engine.register_recipient("owner").unwrap();
    let mut grant = event_grant(UserScope::AllOwned, false);
    grant.owned_windows = true;
    engine.approve_grant(owner, grant).unwrap();
    let id = engine
        .register_owned_window(
            owner,
            WindowObservation {
                bounds: screens().desktop,
                metadata: WindowMetadata {
                    title: Some("Owned window".into()),
                    ..Default::default()
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
    engine.track_window(owner, id).unwrap();
    let valid = engine.snapshot(owner).unwrap();
    let mut invalid = valid.clone();
    invalid.windows[0].metadata.title = Some("x".repeat(513));
    let mut client = ClientState::default();
    assert_eq!(
        client.install_snapshot(invalid, 71),
        Err(FoldError::Invalid("window title"))
    );
    assert!(client.windows().is_empty());
    client.install_snapshot(valid, 71).unwrap();
    assert_eq!(
        client.windows().get(&id).unwrap().metadata.title.as_deref(),
        Some("Owned window")
    );
}

#[test]
fn real_engine_repeated_overflow_keeps_original_origin_and_latest_snapshot_floor() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("shared state consumer").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    let (sub, initial) = subscribe(&mut host, app, 1);
    let origin = initial.through_sequence;
    let mut client = ClientState::default();
    client.install_snapshot(initial, 71).unwrap();
    for x in [1, 2] {
        input(
            &mut host,
            app,
            user,
            Input::Motion {
                position: Point { x, y: 0 },
            },
        )
        .unwrap();
    }
    let first = poll(&mut host, app, sub);
    let Poll::Gap {
        after_sequence,
        lost_through: first_floor,
    } = first.clone()
    else {
        panic!("real Engine overflow");
    };
    assert_eq!(after_sequence, origin);
    assert_eq!(client.fold_poll(first).unwrap(), FoldOutcome::NeedsSnapshot);
    let stale = snapshot(&mut host, app);
    input(
        &mut host,
        app,
        user,
        Input::Motion {
            position: Point { x: 3, y: 0 },
        },
    )
    .unwrap();
    let second = poll(&mut host, app, sub);
    let Poll::Gap {
        after_sequence,
        lost_through: latest,
    } = second.clone()
    else {
        panic!("persisting Engine overflow");
    };
    assert_eq!(after_sequence, origin);
    assert!(latest > first_floor);
    assert_eq!(
        client.fold_poll(second.clone()).unwrap(),
        FoldOutcome::NeedsSnapshot
    );
    assert_eq!(
        client.fold_poll(second).unwrap(),
        FoldOutcome::NeedsSnapshot
    );
    assert!(client.users().is_empty());
    assert_eq!(client.last_sequence(), latest);
    for malformed in [
        Poll::Gap {
            after_sequence: origin,
            lost_through: first_floor,
        },
        Poll::Gap {
            after_sequence: first_floor,
            lost_through: latest + 1,
        },
        Poll::Gap {
            after_sequence: origin,
            lost_through: origin,
        },
    ] {
        assert_eq!(client.fold_poll(malformed), Err(FoldError::Sequence));
        assert_eq!(client.last_sequence(), latest);
    }
    assert_eq!(client.install_snapshot(stale, 71), Err(FoldError::Sequence));
    assert_eq!(client.last_sequence(), latest);
    let response = host
        .dispatch(
            app,
            request(ClientCommand::Resubscribe {
                subscription: sub,
                capacity: 1,
            }),
        )
        .unwrap();
    let ClientResponse::Subscribed {
        subscription: new_sub,
        snapshot: fresh,
    } = response
    else {
        panic!("resubscribe");
    };
    assert_ne!(sub, new_sub);
    client.install_snapshot(fresh, 71).unwrap();
    let new_origin = client.last_sequence();
    for x in [4, 5] {
        input(
            &mut host,
            app,
            user,
            Input::Motion {
                position: Point { x, y: 0 },
            },
        )
        .unwrap();
    }
    let new_gap = poll(&mut host, app, new_sub);
    assert!(matches!(new_gap, Poll::Gap { after_sequence, .. } if after_sequence == new_origin));
    client.fold_poll(new_gap).unwrap();
    client.disconnect();
    assert_eq!(
        client.fold_poll(Poll::Gap {
            after_sequence: new_origin,
            lost_through: latest + 9
        }),
        Err(FoldError::NotLive)
    );
}

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
fn request(command: ClientCommand) -> ClientRequest {
    ClientRequest {
        version: PROTOCOL_VERSION,
        command,
    }
}
fn event_grant(scope: UserScope, keys: bool) -> Grant {
    Grant {
        users: scope,
        pointer_events: true,
        key_events: keys,
        owned_windows: false,
        host_state: false,
    }
}
fn controls(scope: UserScope, delegated: bool) -> ControlGrant {
    ControlGrant {
        users: scope,
        delegated_users: delegated,
        user_lifecycle: true,
        cursor: true,
        pointer_input: true,
        key_input: true,
        window_publication: false,
        window_identity_metadata: false,
        window_tracking: false,
        window_control: false,
        host_mode_control: false,
        host_viewport_control: false,
    }
}
fn snapshot(host: &mut Host, recipient: RecipientId) -> Snapshot {
    match host
        .dispatch(recipient, request(ClientCommand::Snapshot))
        .unwrap()
    {
        ClientResponse::Snapshot { snapshot } => snapshot,
        other => panic!("snapshot expected: {other:?}"),
    }
}
fn subscribe(
    host: &mut Host,
    recipient: RecipientId,
    capacity: usize,
) -> (SubscriptionId, Snapshot) {
    match host
        .dispatch(recipient, request(ClientCommand::Subscribe { capacity }))
        .unwrap()
    {
        ClientResponse::Subscribed {
            subscription,
            snapshot,
        } => (subscription, snapshot),
        other => panic!("subscription expected: {other:?}"),
    }
}
fn poll(host: &mut Host, recipient: RecipientId, subscription: SubscriptionId) -> Poll {
    match host
        .dispatch(
            recipient,
            request(ClientCommand::Poll {
                subscription,
                max_events: MAX_POLL_EVENTS,
            }),
        )
        .unwrap()
    {
        ClientResponse::Polled { poll } => poll,
        other => panic!("poll expected: {other:?}"),
    }
}
fn input(
    host: &mut Host,
    recipient: RecipientId,
    user: UserId,
    input: Input,
) -> Result<ClientResponse, Failure> {
    host.dispatch(recipient, request(ClientCommand::Input { user, input }))
}

#[test]
fn event_export_never_self_grants_controls_or_host_setup() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("consumer").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    assert!(snapshot(&mut host, app).users.is_empty());
    assert_eq!(
        host.dispatch(
            app,
            request(ClientCommand::CreateUser {
                name: "Denied".into()
            })
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    let user = host.create_owned_user(app, "Host created").unwrap();
    assert_eq!(
        input(
            &mut host,
            app,
            user,
            Input::Motion {
                position: Point { x: 0, y: 0 }
            }
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    for forbidden in [
        "approve_events",
        "approve_controls",
        "register_recipient",
        "register_own_window",
        "reset",
        "end",
    ] {
        let json = format!(r#"{{"version":1,"command":{{"kind":"{forbidden}"}}}}"#);
        assert!(serde_json::from_str::<ClientRequest>(&json).is_err());
    }
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Motion {
            position: Point { x: 10, y: 20 },
        },
    )
    .unwrap();
    assert_eq!(
        snapshot(&mut host, app).users[0]
            .pointer
            .as_ref()
            .unwrap()
            .position,
        Point { x: 10, y: 20 }
    );
}

#[test]
fn native_and_browser_observe_and_control_same_two_host_created_users() {
    let mut host = Host::new(71, screens()).unwrap();
    let local = host.register_recipient("local embedding owner").unwrap();
    let browser = host.register_recipient("exact-origin browser").unwrap();
    let a = host.create_owned_user(local, "Blue").unwrap();
    let b = host.create_owned_user(local, "Orange").unwrap();
    let ids = BTreeSet::from([a, b]);
    host.approve_events(local, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(local, controls(UserScope::AllOwned, false))
        .unwrap();
    host.approve_events(browser, event_grant(UserScope::Only(ids.clone()), true))
        .unwrap();
    host.approve_controls(browser, controls(UserScope::Only(ids), true))
        .unwrap();
    for (user, color) in [(a, [30, 120, 255, 255]), (b, [255, 130, 30, 255])] {
        host.dispatch(
            local,
            request(ClientCommand::Cursor {
                user,
                control: CursorControl::Color(color),
            }),
        )
        .unwrap();
    }
    let (native_sub, native_snapshot) = subscribe(&mut host, local, 64);
    let (browser_sub, browser_snapshot) = subscribe(&mut host, browser, 64);
    assert_eq!(
        native_snapshot
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
    assert!(
        native_snapshot
            .users
            .iter()
            .all(|user| user.owned_by_recipient)
    );
    assert!(
        browser_snapshot
            .users
            .iter()
            .all(|user| !user.owned_by_recipient)
    );
    let mut delegated_views = native_snapshot.users.clone();
    for user in &mut delegated_views {
        user.owned_by_recipient = false;
    }
    assert_eq!(browser_snapshot.users, delegated_views);
    let mut native_state = ClientState::default();
    let mut browser_state = ClientState::default();
    native_state.install_snapshot(native_snapshot, 71).unwrap();
    browser_state
        .install_snapshot(browser_snapshot, 71)
        .unwrap();
    for (producer, user, position) in [
        (local, a, Point { x: 10, y: 20 }),
        (browser, b, Point { x: 300, y: 400 }),
    ] {
        input(&mut host, producer, user, Input::Motion { position }).unwrap();
        host.dispatch(
            producer,
            request(ClientCommand::Click {
                user,
                button: MouseButton::Primary,
            }),
        )
        .unwrap();
        for axis in [WheelAxis::Vertical, WheelAxis::Horizontal] {
            input(&mut host, producer, user, Input::Wheel { axis, units: 120 }).unwrap();
        }
        input(
            &mut host,
            producer,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        )
        .unwrap();
        input(
            &mut host,
            producer,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Up,
            },
        )
        .unwrap();
    }
    native_state
        .fold_poll(poll(&mut host, local, native_sub))
        .unwrap();
    browser_state
        .fold_poll(poll(&mut host, browser, browser_sub))
        .unwrap();
    let mut delegated_state = native_state.users().clone();
    for user in delegated_state.values_mut() {
        assert!(user.owned_by_recipient);
        user.owned_by_recipient = false;
    }
    assert_eq!(&delegated_state, browser_state.users());
    assert_eq!(native_state.users().len(), 2);
    assert_ne!(
        native_state.users()[&a].cursor.as_ref().unwrap().color,
        native_state.users()[&b].cursor.as_ref().unwrap().color
    );
    assert_ne!(
        native_state.users()[&a].pointer.as_ref().unwrap().position,
        native_state.users()[&b].pointer.as_ref().unwrap().position
    );
    assert!(
        native_state
            .users()
            .values()
            .all(|user| user.held_keys.as_ref().unwrap().is_empty()
                && user.pointer.as_ref().unwrap().held_buttons.is_empty())
    );
}

#[test]
fn delegated_input_requires_explicit_only_scope_key_consent_and_control_permission() {
    let mut host = Host::new(71, screens()).unwrap();
    let owner = host.register_recipient("owner").unwrap();
    let delegate = host.register_recipient("delegate").unwrap();
    let user = host.create_owned_user(owner, "User").unwrap();
    host.approve_events(owner, event_grant(UserScope::AllOwned, true))
        .unwrap();
    let selected = UserScope::Only(BTreeSet::from([user]));
    host.approve_events(delegate, event_grant(selected.clone(), false))
        .unwrap();
    host.approve_controls(delegate, controls(UserScope::AllOwned, true))
        .unwrap();
    assert_eq!(
        input(
            &mut host,
            delegate,
            user,
            Input::Motion {
                position: Point { x: 0, y: 0 }
            }
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    host.approve_controls(delegate, controls(selected.clone(), false))
        .unwrap();
    assert_eq!(
        input(
            &mut host,
            delegate,
            user,
            Input::Motion {
                position: Point { x: 0, y: 0 }
            }
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    host.approve_controls(delegate, controls(selected.clone(), true))
        .unwrap();
    input(
        &mut host,
        delegate,
        user,
        Input::Motion {
            position: Point { x: 0, y: 0 },
        },
    )
    .unwrap();
    assert_eq!(
        input(
            &mut host,
            delegate,
            user,
            Input::Key {
                key: KeyCode(4),
                transition: Transition::Down
            }
        )
        .unwrap_err()
        .code,
        FailureCode::Denied
    );
    assert!(snapshot(&mut host, delegate).users[0].held_keys.is_none());
    host.approve_events(delegate, event_grant(selected, true))
        .unwrap();
    input(
        &mut host,
        delegate,
        user,
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Down,
        },
    )
    .unwrap();
    let mut no_keys = controls(UserScope::Only(BTreeSet::from([user])), true);
    no_keys.key_input = false;
    host.approve_controls(delegate, no_keys).unwrap();
    assert!(
        snapshot(&mut host, owner).users[0]
            .held_keys
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn hold_cleanup_cannot_release_another_producers_press_after_owner_revoke() {
    let mut host = Host::new(71, screens()).unwrap();
    let owner = host.register_recipient("owner").unwrap();
    let delegate = host.register_recipient("delegate").unwrap();
    let user = host.create_owned_user(owner, "User").unwrap();
    let selected = UserScope::Only(BTreeSet::from([user]));
    host.approve_events(owner, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(owner, controls(UserScope::AllOwned, false))
        .unwrap();
    host.approve_events(delegate, event_grant(selected.clone(), true))
        .unwrap();
    host.approve_controls(delegate, controls(selected, true))
        .unwrap();
    let down = Input::Key {
        key: KeyCode(4),
        transition: Transition::Down,
    };
    let up = Input::Key {
        key: KeyCode(4),
        transition: Transition::Up,
    };
    input(&mut host, delegate, user, down).unwrap();
    assert_eq!(
        input(&mut host, owner, user, up).unwrap_err().code,
        FailureCode::Denied
    );
    host.approve_events(owner, Grant::default()).unwrap(); // Core and host both retire old holds.
    host.approve_events(owner, event_grant(UserScope::AllOwned, true))
        .unwrap();
    input(&mut host, owner, user, down).unwrap();
    host.dispatch(delegate, request(ClientCommand::Disconnect))
        .unwrap();
    assert_eq!(
        snapshot(&mut host, owner).users[0].held_keys,
        Some(vec![KeyCode(4)])
    );
    input(&mut host, owner, user, up).unwrap();
}

#[test]
fn client_accepts_filtered_sequence_skips_and_preserves_cursor_render_fields() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    let other = host.register_recipient("other").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    let _other_user = host.create_owned_user(other, "Other").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    host.dispatch(
        app,
        request(ClientCommand::Cursor {
            user,
            control: CursorControl::Color([1, 2, 3, 255]),
        }),
    )
    .unwrap();
    let (sub, initial) = subscribe(&mut host, app, 16);
    let mut client = ClientState::default();
    client.install_snapshot(initial.clone(), 71).unwrap();
    host.create_owned_user(other, "Filtered lifecycle").unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Motion {
            position: Point { x: 0, y: 0 },
        },
    )
    .unwrap();
    client.fold_poll(poll(&mut host, app, sub)).unwrap();
    assert_eq!(client.last_sequence(), initial.through_sequence + 2);
    assert_eq!(
        client.users()[&user].cursor.as_ref().unwrap().color,
        [1, 2, 3, 255]
    );
    assert_eq!(client.state(), &StreamState::Live);
}

#[test]
fn invalid_batch_is_atomic_and_clears_holds_then_requires_fresh_snapshot() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Down,
        },
    )
    .unwrap();
    let (sub, initial) = subscribe(&mut host, app, 16);
    let mut client = ClientState::default();
    client.install_snapshot(initial.clone(), 71).unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Motion {
            position: Point { x: 30, y: 40 },
        },
    )
    .unwrap();
    let Poll::Events { mut events } = poll(&mut host, app, sub) else {
        panic!("events");
    };
    let duplicate = events[0].clone();
    events.push(duplicate);
    assert_eq!(
        client.fold_poll(Poll::Events { events }).unwrap_err(),
        FoldError::Sequence
    );
    assert!(client.users().is_empty());
    assert_eq!(client.last_sequence(), initial.through_sequence);
    assert_eq!(client.state(), &StreamState::NeedsSnapshot);
    assert_eq!(
        client
            .fold_poll(Poll::Events { events: vec![] })
            .unwrap_err(),
        FoldError::NotLive
    );
    client
        .install_snapshot(snapshot(&mut host, app), 71)
        .unwrap();
    assert_eq!(
        client.users()[&user].pointer.as_ref().unwrap().position,
        Point { x: 30, y: 40 }
    );
    assert_eq!(
        client.install_snapshot(initial, 71).unwrap_err(),
        FoldError::Sequence
    );
}

#[test]
fn client_rejects_an_ownership_hint_change_for_an_existing_user() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("owner").unwrap();
    host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    let initial = snapshot(&mut host, app);
    let mut forged = initial.users[0].clone();
    assert!(forged.owned_by_recipient);
    forged.owned_by_recipient = false;
    let envelope = Envelope {
        version: PROTOCOL_VERSION,
        epoch: 71,
        sequence: initial.through_sequence + 1,
        event: Event::UserUpdated { user: forged },
    };
    let mut client = ClientState::default();
    client.install_snapshot(initial, 71).unwrap();
    assert_eq!(
        client.fold_poll(Poll::Events {
            events: vec![envelope]
        }),
        Err(FoldError::Invalid("user ownership changed"))
    );
    assert_eq!(client.state(), &StreamState::NeedsSnapshot);
    assert!(client.users().is_empty());
}

#[test]
fn gap_rejects_snapshots_before_loss_watermark_and_cannot_restore_old_holds() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Down,
        },
    )
    .unwrap();
    let (sub, stale) = subscribe(&mut host, app, 1);
    let mut client = ClientState::default();
    client.install_snapshot(stale.clone(), 71).unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Up,
        },
    )
    .unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Motion {
            position: Point { x: 30, y: 40 },
        },
    )
    .unwrap();
    let gap = poll(&mut host, app, sub);
    let Poll::Gap { lost_through, .. } = gap.clone() else {
        panic!("expected overflow gap")
    };
    assert_eq!(client.fold_poll(gap).unwrap(), FoldOutcome::NeedsSnapshot);
    assert_eq!(client.last_sequence(), lost_through);
    assert_eq!(
        client.install_snapshot(stale.clone(), 71),
        Err(FoldError::Sequence)
    );
    let mut almost_current = stale;
    almost_current.through_sequence = lost_through - 1;
    assert_eq!(
        client.install_snapshot(almost_current, 71),
        Err(FoldError::Sequence)
    );
    assert!(client.users().is_empty());
    assert_eq!(client.state(), &StreamState::NeedsSnapshot);
    let fresh = snapshot(&mut host, app);
    assert!(fresh.through_sequence >= lost_through);
    client.install_snapshot(fresh, 71).unwrap();
    assert_eq!(client.users()[&user].held_keys, Some(vec![]));
    let mut next_host = Host::new(72, screens()).unwrap();
    let next_app = next_host
        .register_recipient("new authenticated epoch")
        .unwrap();
    next_host
        .approve_events(next_app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    client
        .install_snapshot(snapshot(&mut next_host, next_app), 72)
        .unwrap();
    assert_eq!(client.epoch(), Some(72));
    assert!(client.users().is_empty());
    let watermark = client.last_sequence();
    assert_eq!(
        client.fold_poll(Poll::Gap {
            after_sequence: watermark,
            lost_through: watermark
        }),
        Err(FoldError::Sequence)
    );
    assert_eq!(client.last_sequence(), watermark);
    assert_eq!(client.state(), &StreamState::NeedsSnapshot);
}

#[test]
fn explicit_gap_end_disposal_and_disconnect_remove_held_state_and_stale_cursors() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    let (sub, initial) = subscribe(&mut host, app, 1);
    let mut client = ClientState::default();
    client.install_snapshot(initial, 71).unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Key {
            key: KeyCode(4),
            transition: Transition::Down,
        },
    )
    .unwrap();
    input(
        &mut host,
        app,
        user,
        Input::Button {
            button: MouseButton::X2,
            transition: Transition::Down,
        },
    )
    .unwrap();
    assert_eq!(
        client.fold_poll(poll(&mut host, app, sub)).unwrap(),
        FoldOutcome::NeedsSnapshot
    );
    assert!(client.users().is_empty());
    let response = host
        .dispatch(
            app,
            request(ClientCommand::Resubscribe {
                subscription: sub,
                capacity: 8,
            }),
        )
        .unwrap();
    let ClientResponse::Subscribed {
        subscription: sub,
        snapshot: initial,
    } = response
    else {
        panic!("subscription");
    };
    client.install_snapshot(initial, 71).unwrap();
    host.dispatch(app, request(ClientCommand::DisposeUser { user }))
        .unwrap();
    client.fold_poll(poll(&mut host, app, sub)).unwrap();
    assert!(client.users().is_empty());
    host.end();
    assert_eq!(
        client.fold_poll(poll(&mut host, app, sub)).unwrap(),
        FoldOutcome::Ended(EndReason::SessionEnded)
    );
    assert_eq!(client.state(), &StreamState::Ended(EndReason::SessionEnded));
    client.disconnect();
    assert_eq!(client.state(), &StreamState::Disconnected);
}

#[test]
fn protocol_version_epoch_bad_snapshot_and_credential_debug_are_checked() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    let initial = snapshot(&mut host, app);
    let mut client = ClientState::default();
    assert_eq!(
        client.install_snapshot(initial.clone(), 72).unwrap_err(),
        FoldError::Epoch
    );
    let mut bad = initial.clone();
    bad.version = 99;
    assert_eq!(
        client.install_snapshot(bad, 71).unwrap_err(),
        FoldError::Version
    );
    client.install_snapshot(initial.clone(), 71).unwrap();
    let bad_event = Envelope {
        version: 1,
        epoch: 72,
        sequence: initial.through_sequence + 1,
        event: Event::ScreensChanged { screens: screens() },
    };
    assert_eq!(
        client
            .fold_poll(Poll::Events {
                events: vec![bad_event]
            })
            .unwrap_err(),
        FoldError::Epoch
    );
    assert_eq!(client.state(), &StreamState::NeedsSnapshot);
    let hello = EventHello {
        version: 1,
        credential: "private-event-credential".into(),
    };
    assert!(!format!("{hello:?}").contains("private-event-credential"));
}

#[cfg(feature = "native-ffi")]
mod abi {
    use super::*;
    use crate::{abi_contract::Status, ffi::*};

    fn create() -> u64 {
        let bytes = serde_json::to_vec(&screens()).unwrap();
        let mut handle = 0;
        assert_eq!(
            unsafe { pim_app_host_create(71, bytes.as_ptr(), bytes.len(), &mut handle) },
            Status::Ok as i32
        );
        handle
    }
    fn read(handle: u64) -> serde_json::Value {
        let mut length = 0;
        assert_eq!(
            unsafe { pim_app_response_size(handle, &mut length) },
            Status::Ok as i32
        );
        let mut bytes = vec![0; length];
        assert_eq!(
            unsafe { pim_app_response_read(handle, bytes.as_mut_ptr(), bytes.len(), &mut length) },
            Status::Ok as i32
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn generated_header_and_rust_export_signatures_agree() {
        let _: extern "C" fn() -> u32 = pim_app_abi_version;
        let _: unsafe extern "C" fn(u64, *const u8, usize, *mut u64) -> i32 = pim_app_host_create;
        let _: extern "C" fn(u64) -> i32 = pim_app_host_destroy;
        let _: unsafe extern "C" fn(u64, *const u8, usize) -> i32 = pim_app_host_call;
        let _: unsafe extern "C" fn(u64, u64, *const u8, usize) -> i32 = pim_app_client_call;
        let _: unsafe extern "C" fn(u64, *mut usize) -> i32 = pim_app_response_size;
        let _: unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32 = pim_app_response_read;
        let _: extern "C" fn(u64) -> i32 = pim_app_response_discard;
        let native = cfg!(all(windows, feature = "native-client"));
        assert_eq!(C_HEADER, crate::abi_contract::render_header_for(native));
        assert_eq!(
            C_HEADER.contains("PIM_APP_NATIVE_CLIENT_ABI_VERSION 3u"),
            native
        );
        for function in crate::abi_contract::NATIVE_FUNCTIONS {
            assert_eq!(C_HEADER.matches(function.name).count(), usize::from(native));
        }
        for function in crate::abi_contract::FUNCTIONS {
            assert_eq!(C_HEADER.matches(function.name).count(), 1);
        }
        assert_eq!(pim_app_abi_version(), 1);
    }

    #[test]
    fn abi_small_output_retry_cannot_replay_mutation_and_busy_reply_is_preserved() {
        let handle = create();
        let register = serde_json::to_vec(&HostRequest {
            version: 1,
            command: HostCommand::RegisterRecipient {
                local_identity: "native consumer".into(),
            },
        })
        .unwrap();
        assert_eq!(
            unsafe { pim_app_host_call(handle, register.as_ptr(), register.len()) },
            Status::Ok as i32
        );
        assert_eq!(
            unsafe { pim_app_host_call(handle, register.as_ptr(), register.len()) },
            Status::Busy as i32
        );
        let mut length = 0;
        assert_eq!(
            unsafe { pim_app_response_read(handle, std::ptr::null_mut(), 0, &mut length) },
            Status::BufferTooSmall as i32
        );
        assert!(length > 0);
        let recipient = read(handle)["response"]["recipient"].as_u64().unwrap();
        let create_user = serde_json::to_vec(&HostRequest {
            version: 1,
            command: HostCommand::CreateUser {
                owner: serde_json::from_value(recipient.into()).unwrap(),
                name: "Exactly once".into(),
            },
        })
        .unwrap();
        assert_eq!(
            unsafe { pim_app_host_call(handle, create_user.as_ptr(), create_user.len()) },
            Status::Ok as i32
        );
        let mut tiny = [0_u8; 1];
        assert_eq!(
            unsafe { pim_app_response_read(handle, tiny.as_mut_ptr(), tiny.len(), &mut length) },
            Status::BufferTooSmall as i32
        );
        assert!(read(handle)["response"]["user"]["local"].as_u64().is_some());
        assert_eq!(pim_app_response_discard(handle), Status::NoResponse as i32);
        assert_eq!(pim_app_host_destroy(handle), Status::Ok as i32);
        assert_eq!(pim_app_host_destroy(handle), Status::InvalidHandle as i32);
    }

    #[test]
    fn abi_rejects_invalid_utf8_bounds_wrong_thread_and_contains_panics() {
        let handle = create();
        let bad = [0xff_u8];
        assert_eq!(
            unsafe { pim_app_host_call(handle, bad.as_ptr(), bad.len()) },
            Status::InvalidArgument as i32
        );
        assert_eq!(
            unsafe { pim_app_host_call(handle, bad.as_ptr(), MAX_REQUEST_BYTES + 1) },
            Status::InvalidArgument as i32
        );
        assert_eq!(
            unsafe { pim_app_host_call(handle, std::ptr::null(), 1) },
            Status::InvalidArgument as i32
        );
        assert_eq!(
            std::thread::spawn(move || pim_app_host_destroy(handle))
                .join()
                .unwrap(),
            Status::InvalidHandle as i32
        );
        assert_eq!(panic_for_test(handle), Status::Panic as i32);
        assert_eq!(pim_app_response_discard(handle), Status::Poisoned as i32);
        assert_eq!(pim_app_host_destroy(handle), Status::Ok as i32);
    }
}

#[cfg(feature = "native-ffi")]
#[test]
fn native_abi_uses_same_typed_cooperative_engine_for_two_users_and_client_calls() {
    use crate::{abi_contract::Status, ffi::*};
    let bytes = serde_json::to_vec(&screens()).unwrap();
    let mut handle = 0;
    assert_eq!(
        unsafe { pim_app_host_create(71, bytes.as_ptr(), bytes.len(), &mut handle) },
        Status::Ok as i32
    );
    let read = |handle| {
        let mut length = 0;
        assert_eq!(
            unsafe { pim_app_response_size(handle, &mut length) },
            Status::Ok as i32
        );
        let mut bytes = vec![0_u8; length];
        assert_eq!(
            unsafe { pim_app_response_read(handle, bytes.as_mut_ptr(), bytes.len(), &mut length) },
            Status::Ok as i32
        );
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    let host_call = |command| {
        let bytes = serde_json::to_vec(&HostRequest {
            version: 1,
            command,
        })
        .unwrap();
        assert_eq!(
            unsafe { pim_app_host_call(handle, bytes.as_ptr(), bytes.len()) },
            Status::Ok as i32
        );
        read(handle)
    };
    let registered = host_call(HostCommand::RegisterRecipient {
        local_identity: "one native process".into(),
    });
    let recipient_number = registered["response"]["recipient"].as_u64().unwrap();
    let recipient: RecipientId = serde_json::from_value(recipient_number.into()).unwrap();
    host_call(HostCommand::ApproveEvents {
        recipient,
        grant: event_grant(UserScope::AllOwned, true),
    });
    host_call(HostCommand::ApproveControls {
        recipient,
        grant: controls(UserScope::AllOwned, false),
    });
    let a: UserId = serde_json::from_value(
        host_call(HostCommand::CreateUser {
            owner: recipient,
            name: "Blue".into(),
        })["response"]["user"]
            .clone(),
    )
    .unwrap();
    let b: UserId = serde_json::from_value(
        host_call(HostCommand::CreateUser {
            owner: recipient,
            name: "Orange".into(),
        })["response"]["user"]
            .clone(),
    )
    .unwrap();
    assert_ne!(a, b);
    let client_call = |command| {
        let bytes = serde_json::to_vec(&request(command)).unwrap();
        assert_eq!(
            unsafe { pim_app_client_call(handle, recipient_number, bytes.as_ptr(), bytes.len()) },
            Status::Ok as i32
        );
        read(handle)
    };
    let subscribed = client_call(ClientCommand::Subscribe { capacity: 16 });
    assert_eq!(
        subscribed["response"]["snapshot"]["users"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let subscription: SubscriptionId =
        serde_json::from_value(subscribed["response"]["subscription"].clone()).unwrap();
    client_call(ClientCommand::Input {
        user: a,
        input: Input::Motion {
            position: Point { x: 10, y: 20 },
        },
    });
    client_call(ClientCommand::Input {
        user: b,
        input: Input::Motion {
            position: Point { x: 300, y: 400 },
        },
    });
    let polled = client_call(ClientCommand::Poll {
        subscription,
        max_events: 16,
    });
    assert_eq!(
        polled["response"]["poll"]["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(pim_app_host_destroy(handle), Status::Ok as i32);
}

#[cfg(feature = "native-ffi")]
#[test]
fn native_abi_caps_hosts_per_thread_and_releases_all_slots_on_destroy() {
    use crate::{
        abi_contract::{MAX_HOSTS_PER_THREAD, Status},
        ffi::*,
    };
    let bytes = serde_json::to_vec(&screens()).unwrap();
    let mut handles = Vec::new();
    for _ in 0..MAX_HOSTS_PER_THREAD {
        let mut handle = 0;
        assert_eq!(
            unsafe { pim_app_host_create(71, bytes.as_ptr(), bytes.len(), &mut handle) },
            Status::Ok as i32
        );
        handles.push(handle);
    }
    let mut untouched = u64::MAX;
    assert_eq!(
        unsafe { pim_app_host_create(71, bytes.as_ptr(), bytes.len(), &mut untouched) },
        Status::Limit as i32
    );
    assert_eq!(untouched, u64::MAX);
    for handle in handles {
        assert_eq!(pim_app_host_destroy(handle), Status::Ok as i32);
    }
    let mut handle = 0;
    assert_eq!(
        unsafe { pim_app_host_create(72, bytes.as_ptr(), bytes.len(), &mut handle) },
        Status::Ok as i32
    );
    assert_eq!(pim_app_host_destroy(handle), Status::Ok as i32);
}

#[test]
fn coordinate_revision_is_checked_at_host_acceptance_before_input_mutation() {
    let mut host = Host::new(71, screens()).unwrap();
    let app = host.register_recipient("app").unwrap();
    let user = host.create_owned_user(app, "User").unwrap();
    host.approve_events(app, event_grant(UserScope::AllOwned, true))
        .unwrap();
    host.approve_controls(app, controls(UserScope::AllOwned, false))
        .unwrap();
    let mut next = screens();
    next.revision = 2;
    host.update_screens(next).unwrap();
    let before = snapshot(&mut host, app);
    let input = Input::Motion {
        position: Point { x: 10, y: 20 },
    };
    let stale = host
        .dispatch(
            app,
            request(ClientCommand::InputAtRevision {
                user,
                input,
                transform_revision: 1,
            }),
        )
        .unwrap_err();
    assert_eq!(stale.code, FailureCode::StaleRevision);
    assert_eq!(snapshot(&mut host, app), before);
    host.dispatch(
        app,
        request(ClientCommand::InputAtRevision {
            user,
            input,
            transform_revision: 2,
        }),
    )
    .unwrap();
    assert_eq!(
        snapshot(&mut host, app).users[0]
            .pointer
            .as_ref()
            .unwrap()
            .position,
        Point { x: 10, y: 20 }
    );
}

fn hosted_model(revision: u64) -> HostedState {
    HostedState {
        revision,
        routing_generation: 1,
        phase: HostedPhase::Ready,
        mode: Some(HostedMode::Concurrent),
        multi_keyboard: Some(true),
        minimized: Some(false),
    }
}

#[test]
fn hosted_query_and_standalone_actions_do_not_fabricate_gui_effects() {
    let mut host: Host = Host::new(71, screens()).unwrap();
    let reader = host.register_recipient("host observer").unwrap();
    host.approve_events(
        reader,
        Grant {
            host_state: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        host.dispatch(
            reader,
            request(ClientCommand::HostedStateQuery { epoch: 71 })
        )
        .unwrap_err()
        .code,
        FailureCode::Unsupported
    );
    host.publish_hosted_state(hosted_model(1)).unwrap();
    assert_eq!(
        host.dispatch(
            reader,
            request(ClientCommand::HostedStateQuery { epoch: 71 })
        )
        .unwrap(),
        ClientResponse::HostedState {
            state: hosted_model(1)
        }
    );
    host.approve_controls(
        reader,
        ControlGrant {
            host_mode_control: true,
            host_viewport_control: true,
            ..Default::default()
        },
    )
    .unwrap();
    for command in [
        ClientCommand::HostedSetMode {
            reference: HostRevision {
                epoch: 71,
                revision: 1,
            },
            mode: HostedMode::Native,
        },
        ClientCommand::HostedViewport {
            reference: HostRevision {
                epoch: 71,
                revision: 1,
            },
            action: HostViewportAction::Minimize,
        },
    ] {
        assert_eq!(
            host.dispatch(reader, request(command)).unwrap_err().code,
            FailureCode::Unsupported
        );
        assert_eq!(host.hosted_state(reader, 71).unwrap(), hosted_model(1));
    }
    assert_eq!(
        host.dispatch(
            reader,
            request(ClientCommand::HostedViewport {
                reference: HostRevision {
                    epoch: 71,
                    revision: 2
                },
                action: HostViewportAction::Restore
            })
        )
        .unwrap_err()
        .code,
        FailureCode::StaleRevision
    );
}

#[test]
fn hosted_folding_clears_on_gap_disconnect_and_rejects_revision_rollback() {
    let mut host: Host = Host::new(71, screens()).unwrap();
    let reader = host.register_recipient("host fold observer").unwrap();
    host.approve_events(
        reader,
        Grant {
            host_state: true,
            ..Default::default()
        },
    )
    .unwrap();
    host.publish_hosted_state(hosted_model(1)).unwrap();
    let mut client = ClientState::default();
    client
        .install_snapshot(snapshot(&mut host, reader), 71)
        .unwrap();
    assert_eq!(client.hosted(), Some(&hosted_model(1)));
    let sequence = client.last_sequence() + 1;
    assert!(
        client
            .fold_poll(Poll::Events {
                events: vec![Envelope {
                    version: PROTOCOL_VERSION,
                    epoch: 71,
                    sequence,
                    event: Event::HostedStateChanged {
                        state: hosted_model(1)
                    }
                }]
            })
            .is_err()
    );
    assert!(client.hosted().is_none());
    client
        .install_snapshot(snapshot(&mut host, reader), 71)
        .unwrap();
    let sequence = client.last_sequence();
    client
        .fold_poll(Poll::Gap {
            after_sequence: sequence,
            lost_through: sequence + 1,
        })
        .unwrap();
    assert!(client.hosted().is_none());
    host.publish_hosted_state(hosted_model(2)).unwrap();
    client
        .install_snapshot(snapshot(&mut host, reader), 71)
        .unwrap();
    client.disconnect();
    assert!(client.hosted().is_none());
}

#[test]
fn hosted_grants_default_off_and_omit_new_false_fields_for_strict_old_ready() {
    let events = serde_json::to_value(Grant::default()).unwrap();
    let controls = serde_json::to_value(ControlGrant::default()).unwrap();
    assert!(events.get("host_state").is_none());
    assert!(controls.get("host_mode_control").is_none());
    assert!(controls.get("host_viewport_control").is_none());
    assert!(!serde_json::from_value::<Grant>(events).unwrap().host_state);
    let controls: ControlGrant = serde_json::from_value(controls).unwrap();
    assert!(!controls.host_mode_control && !controls.host_viewport_control);
    assert!(capabilities().len() <= 32);
    assert!(
        capabilities()
            .iter()
            .any(|entry| entry.capability == Capability::HostVisibility && !entry.available)
    );
}

#[test]
fn default_ready_remains_readable_by_strict_legacy_controls() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyControls {
        users: UserScope,
        delegated_users: bool,
        user_lifecycle: bool,
        cursor: bool,
        pointer_input: bool,
        key_input: bool,
        window_tracking: bool,
        window_control: bool,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyReady {
        version: u32,
        epoch: u64,
        recipient: RecipientId,
        events: Grant,
        controls: LegacyControls,
        capabilities: Vec<CapabilityStatus>,
        limits: EventLimits,
    }
    let mut host: Host = Host::new(71, screens()).unwrap();
    let recipient = host.register_recipient("legacy ready reader").unwrap();
    host.approve_events(recipient, event_grant(UserScope::AllOwned, false))
        .unwrap();
    let ready = host.ready_for(recipient).unwrap();
    let legacy: LegacyReady =
        serde_json::from_value(serde_json::to_value(&ready).unwrap()).unwrap();
    assert_eq!(legacy.version, ready.version);
    assert_eq!(legacy.epoch, ready.epoch);
    assert_eq!(legacy.recipient, ready.recipient);
    assert_eq!(legacy.events, ready.events);
    assert_eq!(legacy.capabilities, ready.capabilities);
    assert_eq!(legacy.limits, ready.limits);
    assert_eq!(legacy.controls.users, UserScope::Only(Default::default()));
    assert!(
        !legacy.controls.delegated_users
            && !legacy.controls.user_lifecycle
            && !legacy.controls.cursor
            && !legacy.controls.pointer_input
            && !legacy.controls.key_input
            && !legacy.controls.window_tracking
            && !legacy.controls.window_control
    );
}

/// This is the pre-hosted wire enum, intentionally independent of Capability.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyCapability {
    CooperativeVirtualInput,
    PortableCursorState,
    OwnWindowAdapter,
    NativeRecipientTransport,
    BrowserTransport,
    SystemCursor,
    CursorThemes,
    CursorFlags,
    HostVisibility,
    TargetInjection,
    PhysicalInput,
    Pen,
    Touch,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCapabilityStatus {
    capability: LegacyCapability,
    available: bool,
    detail: String,
}

#[test]
fn standalone_ready_and_snapshot_keep_the_pre_hosted_capability_schema() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacySnapshot {
        version: u32,
        epoch: u64,
        through_sequence: u64,
        users: Vec<UserView>,
        screens: ScreenState,
        windows: Vec<WindowInfo>,
        capabilities: Vec<LegacyCapabilityStatus>,
        mode: ApplicationMode,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyReady {
        version: u32,
        epoch: u64,
        recipient: RecipientId,
        events: serde_json::Value,
        controls: serde_json::Value,
        capabilities: Vec<LegacyCapabilityStatus>,
        limits: EventLimits,
    }
    let mut host: Host = Host::new(71, screens()).unwrap();
    let ordinary = host
        .register_recipient("legacy capability observer")
        .unwrap();
    host.approve_events(ordinary, event_grant(UserScope::AllOwned, false))
        .unwrap();
    let provider = host.register_recipient("private desktop provider").unwrap();
    let observed = host.create_desktop_observed(provider, "Observed", DesktopBaselineV1 {
        version: 1, next_sequence: 1, screens: screens(), position: Point { x: 1, y: 1 },
        cursor: ConfiguredDesktopCursor { shape: CursorShape::System, size: 32.5,
            color: [1, 2, 3], visible: true, follow_application: false },
        held_buttons: Vec::new(), native_held_keys: Vec::new(),
    }).unwrap();
    let cooperative = host.create_owned_user(ordinary, "Cooperative").unwrap();
    host.approve_events(ordinary, event_grant(UserScope::Only(BTreeSet::from([observed, cooperative])), false)).unwrap();
    host.approve_desktop_observation(ordinary, Some(DesktopConsentV1 { version: 1, text_units: false })).unwrap();
    // Consent without decoder negotiation still uses the actual legacy decoder below.
    // Trusted publication must not leak a new variant to existing recipients.
    host.publish_hosted_state(hosted_model(1)).unwrap();
    let current = snapshot(&mut host, ordinary);
    assert!(current.hosted.is_none());
    assert_eq!(current.users.len(), 1);
    assert_eq!(current.users[0].id, cooperative);
    assert!(serde_json::to_value(&current.users[0]).unwrap().get("desktop").is_none());
    let legacy: LegacySnapshot =
        serde_json::from_value(serde_json::to_value(&current).unwrap()).unwrap();
    assert_eq!(legacy.version, current.version);
    assert_eq!(legacy.epoch, current.epoch);
    assert_eq!(legacy.through_sequence, current.through_sequence);
    assert_eq!(legacy.users, current.users);
    assert_eq!(legacy.screens, current.screens);
    assert_eq!(legacy.windows, current.windows);
    assert_eq!(legacy.mode, ApplicationMode::Cooperative);
    assert_eq!(legacy.capabilities.len(), current.capabilities.len());
    let ready = host.ready_for(ordinary).unwrap();
    let legacy: LegacyReady =
        serde_json::from_value(serde_json::to_value(&ready).unwrap()).unwrap();
    assert_eq!(legacy.version, ready.version);
    assert_eq!(legacy.epoch, ready.epoch);
    assert_eq!(legacy.recipient, ordinary);
    assert_eq!(legacy.events, serde_json::to_value(&ready.events).unwrap());
    assert_eq!(
        legacy.controls,
        serde_json::to_value(&ready.controls).unwrap()
    );
    assert_eq!(legacy.limits, ready.limits);
    assert_eq!(legacy.capabilities.len(), 13);
    for status in legacy.capabilities {
        assert!(!status.detail.is_empty());
        if matches!(status.capability, LegacyCapability::HostVisibility) {
            assert!(!status.available);
        }
    }
    // Even explicit host-state consent does not invent a standalone executor.
    let hosted = host.register_recipient("standalone host reader").unwrap();
    host.approve_events(
        hosted,
        Grant {
            host_state: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        host.ready_for(hosted)
            .unwrap()
            .capabilities
            .iter()
            .all(|entry| entry.capability != Capability::HostedRouting)
    );
    assert!(
        snapshot(&mut host, hosted)
            .capabilities
            .iter()
            .all(|entry| entry.capability != Capability::HostedRouting)
    );
    assert!(
        capabilities()
            .iter()
            .all(|entry| entry.capability != Capability::HostedRouting)
    );
}
