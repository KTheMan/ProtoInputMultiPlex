//! Source-authored deterministic tests: no sockets, helper process, credential IO
//! or product launch. Actual loopback/browser/native acceptance is a separate gate.
use super::{
    authority::*,
    bridge::*,
    network::{authenticate_hello, encode, origin_allowed},
};
use mux_application_sdk::*;
use mux_core::desktop::Bounds;
use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{handshake::server::Request, http};

const ORIGIN: &str = "https://consumer.example";
fn controls(users: UserScope, delegated_users: bool) -> ControlGrant {
    ControlGrant {
        users,
        delegated_users,
        user_lifecycle: false,
        cursor: true,
        pointer_input: true,
        key_input: true,
        window_tracking: false,
        window_control: false,
        window_publication: false,
        window_identity_metadata: false,
    }
}
fn events(users: UserScope) -> Grant {
    Grant {
        users,
        pointer_events: true,
        key_events: true,
        owned_windows: false,
    }
}
fn setup() -> (
    HostBridge,
    NetworkEndpoint,
    RecipientId,
    RecipientId,
    UserId,
    UserId,
) {
    let bounds = Bounds {
        left: -100,
        top: -50,
        right: 1900,
        bottom: 950,
    };
    let mut host = Host::<PlatformWindows>::new(
        71,
        ScreenState {
            revision: 1,
            desktop: bounds,
            screens: vec![bounds],
        },
    )
    .unwrap();
    let local = host.register_recipient("local cooperative owner").unwrap();
    let browser = host.register_recipient("event browser receiver").unwrap();
    let a = host.create_owned_user(local, "Blue").unwrap();
    let b = host.create_owned_user(local, "Orange").unwrap();
    host.approve_events(local, events(UserScope::AllOwned))
        .unwrap();
    host.approve_controls(local, controls(UserScope::AllOwned, false))
        .unwrap();
    let selected = UserScope::Only(BTreeSet::from([a, b]));
    host.approve_events(browser, events(selected.clone()))
        .unwrap();
    host.approve_controls(browser, controls(selected, true))
        .unwrap();
    let (bridge, endpoint) = HostBridge::new(host);
    (bridge, endpoint, local, browser, a, b)
}
fn snapshot(bridge: &mut HostBridge, recipient: RecipientId) -> Snapshot {
    match bridge
        .dispatch_local(
            recipient,
            ClientRequest {
                version: 1,
                command: ClientCommand::Snapshot,
            },
        )
        .unwrap()
    {
        ClientResponse::Snapshot { snapshot } => snapshot,
        other => panic!("expected snapshot: {other:?}"),
    }
}
fn motion(bridge: &mut HostBridge, local: RecipientId, user: UserId, x: i32) {
    bridge
        .dispatch_local(
            local,
            ClientRequest {
                version: 1,
                command: ClientCommand::Input {
                    user,
                    input: Input::Motion {
                        position: Point { x, y: 0 },
                    },
                },
            },
        )
        .unwrap();
}
fn permit(bridge: &mut HostBridge, browser: RecipientId) -> ClientPermit {
    bridge
        .approve_client(browser, ORIGIN, Duration::from_secs(30))
        .unwrap()
}
fn connect(
    bridge: &mut HostBridge,
    endpoint: &NetworkEndpoint,
    permit: &ClientPermit,
) -> mpsc::Receiver<ServerMessage> {
    let (output, receiver) = mpsc::channel(OUT_CAPACITY);
    let (ready, mut response) = oneshot::channel();
    endpoint
        .requests
        .try_send(UiRequest::Connect {
            lease: Arc::clone(&permit.lease),
            output,
            ready,
        })
        .unwrap();
    bridge.tick();
    let ready = response.try_recv().unwrap().unwrap();
    assert_eq!(ready.recipient, permit.lease.recipient);
    assert!(
        ready
            .capabilities
            .iter()
            .any(|status| status.capability == Capability::BrowserTransport && status.available)
    );
    receiver
}
fn command(
    endpoint: &NetworkEndpoint,
    permit: &ClientPermit,
    id: u64,
    command: ClientCommand,
) -> Arc<Admission> {
    let admission = Arc::new(Admission::new());
    endpoint
        .requests
        .try_send(UiRequest::Command {
            lease: Arc::clone(&permit.lease),
            message: ClientMessage {
                request_id: id,
                request: ClientRequest {
                    version: 1,
                    command,
                },
            },
            deadline: Instant::now() + REQUEST_TIMEOUT,
            admission: Arc::clone(&admission),
        })
        .unwrap();
    admission
}
fn install_test_credential(permit: &ClientPermit, byte: char) -> String {
    let credential = byte.to_string().repeat(64);
    permit.lease.issued.store(true, Ordering::Release);
    *permit.lease.credential.lock().unwrap() = Some(credential.clone());
    credential
}

#[test]
fn gui_remote_window_requires_exact_delegation_and_reports_lost_control_receipt_as_uncertain() {
    let bounds = Bounds { left: 0, top: 0, right: 640, bottom: 480 };
    let mut host = Host::<PlatformWindows>::new(71, ScreenState { revision: 1, desktop: bounds, screens: vec![bounds] }).unwrap();
    let publisher = host.register_recipient("native publisher").unwrap();
    let browser = host.register_recipient("browser window receiver").unwrap();
    let grant = Grant {
        owned_windows: true,
        ..Default::default()
    };
    host.approve_events(publisher, grant.clone()).unwrap();
    host.approve_events(browser, grant).unwrap();
    host.approve_controls(publisher, ControlGrant { window_publication: true, window_tracking: true, ..Default::default() }).unwrap();
    host.approve_controls(browser, ControlGrant { window_tracking: true, window_control: true, ..Default::default() }).unwrap();
    let (mut bridge, endpoint) = HostBridge::new(host);
    let owner_permit = permit(&mut bridge, publisher);
    let mut owner_output = connect(&mut bridge, &endpoint, &owner_permit);
    let observed = WindowObservation {
        bounds,
        metadata: WindowMetadata { process_id: Some(42), ui_thread_id: Some(7), ..Default::default() },
        visible: true,
        minimized: false,
        maximized: false,
        click_through: false,
        enabled: true,
        topmost: false,
        opacity: 255,
    };
    let admission = Arc::new(Admission::new());
    endpoint.requests.try_send(UiRequest::Owner {
        lease: Arc::clone(&owner_permit.lease),
        frame: OwnerFrame { request_id: 1, operation: OwnerOperation::Register { token: 9, generation: 1, observed: observed.clone(), identity_metadata_consent: true } },
        deadline: Instant::now() + REQUEST_TIMEOUT,
        admission,
    }).unwrap();
    bridge.tick();
    let ServerMessage::OwnerResponse { reply: Reply::Ok { response: OwnerResponse::Registered { window }, .. }, .. } = owner_output.try_recv().unwrap() else {
        panic!("registered owner window");
    };
    assert_eq!(bridge.window_inventory().len(), 1);
    assert_eq!(bridge.window_inventory()[0].0.metadata.process_id, None);
    assert_eq!(bridge.dispatch_local(browser, ClientRequest { version: PROTOCOL_VERSION, command: ClientCommand::RemoteWindowQuery { reference: RemoteWindowRef { epoch: 71, window } } }).unwrap_err().code, FailureCode::Denied);
    bridge.host_command(HostRequest { version: PROTOCOL_VERSION, command: HostCommand::DelegateWindow { window, recipient: browser, access: WindowAccess { observe: true, control: true } } }).unwrap();
    let browser_permit = permit(&mut bridge, browser);
    let mut browser_output = connect(&mut bridge, &endpoint, &browser_permit);
    command(&endpoint, &browser_permit, 1, ClientCommand::RemoteWindowQuery { reference: RemoteWindowRef { epoch: 70, window } });
    bridge.tick();
    assert!(matches!(browser_output.try_recv().unwrap(), ServerMessage::Response { reply: Reply::Error { error: Failure { code: FailureCode::StaleRevision, .. }, .. }, .. }));
    command(&endpoint, &browser_permit, 2, ClientCommand::RemoteWindowTrack { reference: RemoteWindowRef { epoch: 71, window } });
    bridge.tick();
    assert!(matches!(browser_output.try_recv().unwrap(), ServerMessage::Response { reply: Reply::Ok { response: ClientResponse::Window { .. }, .. }, .. }));
    command(&endpoint, &browser_permit, 3, ClientCommand::RemoteWindowControl { reference: RemoteWindowRef { epoch: 71, window }, control: WindowControl::Hide });
    bridge.tick();
    assert!(matches!(owner_output.try_recv().unwrap(), ServerMessage::WindowApply { window: applied, .. } if applied == window));
    assert!(browser_output.try_recv().is_err());
    bridge.tick_at(Instant::now() + REQUEST_TIMEOUT);
    assert!(matches!(browser_output.try_recv().unwrap(), ServerMessage::Response { reply: Reply::Error { error: Failure { code: FailureCode::Uncertain, .. }, .. }, .. }));
    assert!(bridge.window_inventory()[0].0.observation.stale);
    assert_eq!(bridge.window_status().pending, 0);
}

#[test]
fn origin_path_host_and_upgrade_auth_shape_are_exact_and_mandatory() {
    let origins = vec![ORIGIN.to_owned()];
    validate_origins(&origins).unwrap();
    for invalid in [
        vec![],
        vec!["*".into()],
        vec!["https://consumer.example/".into()],
        vec!["null".into()],
        vec![ORIGIN.into(), ORIGIN.into()],
    ] {
        assert!(validate_origins(&invalid).is_err());
    }
    let make = |origin: Option<&str>, host: &str, path: &str| {
        let mut builder = http::Request::builder().uri(path).header("host", host);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder.body(()).unwrap()
    };
    assert_eq!(
        origin_allowed(
            &make(Some(ORIGIN), "127.0.0.1:40000", "/application-events/v1"),
            &origins,
            40000
        ),
        Some(ORIGIN.into())
    );
    for request in [
        make(None, "127.0.0.1:40000", "/application-events/v1"),
        make(
            Some("https://wrong.example"),
            "127.0.0.1:40000",
            "/application-events/v1",
        ),
        make(Some(ORIGIN), "localhost:40000", "/application-events/v1"),
        make(Some(ORIGIN), "127.0.0.1:40001", "/application-events/v1"),
        make(
            Some(ORIGIN),
            "127.0.0.1:40000",
            "/application-events/v1?credential=ignored",
        ),
        make(Some(ORIGIN), "127.0.0.1:40000", "/mcp"),
    ] {
        assert!(origin_allowed(&request, &origins, 40000).is_none());
    }
    let duplicate: Request = http::Request::builder()
        .uri("/application-events/v1")
        .header("host", "127.0.0.1:40000")
        .header("origin", ORIGIN)
        .header("origin", ORIGIN)
        .body(())
        .unwrap();
    assert!(origin_allowed(&duplicate, &origins, 40000).is_none());
    let bearer: Request = http::Request::builder()
        .uri("/application-events/v1")
        .header("host", "127.0.0.1:40000")
        .header("origin", ORIGIN)
        .header("authorization", "Bearer not-an-event-credential")
        .body(())
        .unwrap();
    assert!(origin_allowed(&bearer, &origins, 40000).is_none());
}

#[test]
fn hello_requires_fresh_event_only_credential_origin_version_and_single_connection() {
    let (mut bridge, endpoint, local, browser, _, _) = setup();
    assert!(
        authenticate_hello(
            &endpoint.shared,
            ORIGIN,
            &EventHello {
                version: 1,
                credential: "a".repeat(64)
            },
            Instant::now()
        )
        .is_err()
    );
    let approved = permit(&mut bridge, browser);
    let token = install_test_credential(&approved, 'a');
    for (origin, hello) in [
        (
            ORIGIN,
            EventHello {
                version: 2,
                credential: token.clone(),
            },
        ),
        (
            ORIGIN,
            EventHello {
                version: 1,
                credential: "b".repeat(64),
            },
        ),
        (
            "https://wrong.example",
            EventHello {
                version: 1,
                credential: token.clone(),
            },
        ),
    ] {
        assert!(authenticate_hello(&endpoint.shared, origin, &hello, Instant::now()).is_err());
    }
    let hello = EventHello {
        version: 1,
        credential: token,
    };
    let lease = authenticate_hello(&endpoint.shared, ORIGIN, &hello, Instant::now()).unwrap();
    assert_eq!(lease.recipient, browser);
    assert!(authenticate_hello(&endpoint.shared, ORIGIN, &hello, Instant::now()).is_err());
    assert_eq!(snapshot(&mut bridge, local).users.len(), 2); // Authentication creates no users.
    lease.revoke();
    assert!(authenticate_hello(&endpoint.shared, ORIGIN, &hello, Instant::now()).is_err());
}

#[test]
fn expiry_rejects_before_ui_application_and_releases_only_browser_produced_holds() {
    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let now = Instant::now();
    let approved = bridge
        .approve_client_at(browser, ORIGIN, Duration::from_millis(100), now)
        .unwrap();
    let _output = connect(&mut bridge, &endpoint, &approved);
    command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Input {
            user,
            input: Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        },
    );
    bridge.tick();
    assert_eq!(
        snapshot(&mut bridge, local).users[0].held_keys,
        Some(vec![KeyCode(4)])
    );
    let queued = command(
        &endpoint,
        &approved,
        2,
        ClientCommand::Input {
            user,
            input: Input::Motion {
                position: Point { x: 999, y: 0 },
            },
        },
    );
    bridge.tick_at(now + Duration::from_millis(101));
    assert_eq!(queued.state.load(Ordering::Acquire), CANCELLED);
    assert!(approved.lease.cleanup_done.load(Ordering::Acquire));
    let state = snapshot(&mut bridge, local);
    assert!(state.users[0].held_keys.as_ref().unwrap().is_empty());
    assert_ne!(state.users[0].pointer.as_ref().unwrap().position.x, 999);
    assert_eq!(state.users.len(), 2);
}

#[test]
fn snapshot_reply_precedes_pushed_activity_and_idle_ticks_emit_nothing() {
    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let approved = permit(&mut bridge, browser);
    let mut output = connect(&mut bridge, &endpoint, &approved);
    command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Subscribe { capacity: 16 },
    );
    bridge.tick();
    motion(&mut bridge, local, user, 100);
    bridge.tick();
    let ServerMessage::Response {
        request_id: 1,
        reply:
            Reply::Ok {
                response:
                    ClientResponse::Subscribed {
                        subscription,
                        snapshot,
                    },
                ..
            },
    } = output.try_recv().unwrap()
    else {
        panic!("initial snapshot reply must be first");
    };
    let ServerMessage::EventStream {
        subscription: streamed,
        stream: Poll::Events { events },
    } = output.try_recv().unwrap()
    else {
        panic!("activity must be pushed");
    };
    assert_eq!(subscription, streamed);
    assert_eq!(events.len(), 1);
    assert!(events[0].sequence > snapshot.through_sequence);
    let mut state = ClientState::default();
    state.install_snapshot(snapshot, 71).unwrap();
    state.fold_poll(Poll::Events { events }).unwrap();
    assert_eq!(
        state.users()[&user].pointer.as_ref().unwrap().position.x,
        100
    );
    bridge.tick();
    assert!(output.try_recv().is_err());
}

#[test]
fn cancelled_or_expired_pending_commands_are_never_replayed_by_the_ui() {
    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let approved = permit(&mut bridge, browser);
    let _output = connect(&mut bridge, &endpoint, &approved);
    let admission = command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Input {
            user,
            input: Input::Motion {
                position: Point { x: 900, y: 0 },
            },
        },
    );
    assert!(admission.cancel_queued());
    bridge.tick();
    assert_ne!(
        snapshot(&mut bridge, local).users[0]
            .pointer
            .as_ref()
            .unwrap()
            .position
            .x,
        900
    );
    assert_eq!(endpoint.shared.uncertain.load(Ordering::Acquire), 0);
    let applying = Admission::new();
    applying.state.store(APPLYING, Ordering::Release);
    assert!(!applying.cancel_queued());
    applying.uncertain(&endpoint.shared);
    applying.uncertain(&endpoint.shared);
    assert_eq!(endpoint.shared.uncertain.load(Ordering::Acquire), 1);
}

#[test]
fn queue_capacity_is_explicit_and_slow_client_fence_blocks_late_commands() {
    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let approved = permit(&mut bridge, browser);
    let mut output = connect(&mut bridge, &endpoint, &approved);
    command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Subscribe { capacity: 16 },
    );
    bridge.tick();
    // Leave the snapshot and pushed frames queued to exercise bounded backpressure.
    for x in 0..OUT_CAPACITY {
        motion(&mut bridge, local, user, x as i32);
        bridge.tick();
    }
    assert!(!approved.lease.allowed.load(Ordering::Acquire));
    let late = command(
        &endpoint,
        &approved,
        2,
        ClientCommand::Input {
            user,
            input: Input::Motion {
                position: Point { x: 999, y: 0 },
            },
        },
    );
    bridge.tick();
    assert_eq!(late.state.load(Ordering::Acquire), CANCELLED);
    assert_eq!(
        snapshot(&mut bridge, local).users[0]
            .pointer
            .as_ref()
            .unwrap()
            .position
            .x,
        (OUT_CAPACITY - 1) as i32
    );
    while output.try_recv().is_ok() {}
    let mut queued = Vec::new();
    for id in 1..=UI_CAPACITY {
        let admission = Arc::new(Admission::new());
        endpoint
            .requests
            .try_send(UiRequest::Command {
                lease: Arc::clone(&approved.lease),
                message: ClientMessage {
                    request_id: id as u64 + 10,
                    request: ClientRequest {
                        version: 1,
                        command: ClientCommand::Snapshot,
                    },
                },
                deadline: Instant::now() + REQUEST_TIMEOUT,
                admission: Arc::clone(&admission),
            })
            .unwrap();
        queued.push(admission);
    }
    let (output, _receive) = mpsc::channel(1);
    let (ready, _receive) = oneshot::channel();
    assert!(matches!(
        endpoint.requests.try_send(UiRequest::Connect {
            lease: Arc::clone(&approved.lease),
            output,
            ready
        }),
        Err(mpsc::error::TrySendError::Full(_))
    ));
    bridge.stop();
    assert!(
        queued
            .iter()
            .all(|admission| admission.state.load(Ordering::Acquire) == CANCELLED)
    );
}

#[test]
fn revocation_retires_receiver_and_new_approval_keeps_same_two_local_user_ids() {
    let (mut bridge, endpoint, local, browser, a, b) = setup();
    let approved = permit(&mut bridge, browser);
    let token = install_test_credential(&approved, 'a');
    let _output = connect(&mut bridge, &endpoint, &approved);
    let result = bridge.host_command(HostRequest {
        version: 1,
        command: HostCommand::ApproveControls {
            recipient: browser,
            grant: ControlGrant::default(),
        },
    });
    assert_eq!(result.unwrap_err().code, FailureCode::Ended);
    assert!(approved.lease.cleanup_done.load(Ordering::Acquire));
    assert!(
        authenticate_hello(
            &endpoint.shared,
            ORIGIN,
            &EventHello {
                version: 1,
                credential: token
            },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(
        snapshot(&mut bridge, local)
            .users
            .iter()
            .map(|user| user.id)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
    let HostResponse::RecipientRegistered { recipient } = bridge
        .host_command(HostRequest {
            version: 1,
            command: HostCommand::RegisterRecipient {
                local_identity: "replacement event receiver".into(),
            },
        })
        .unwrap()
    else {
        panic!("registered receiver");
    };
    let scope = UserScope::Only(BTreeSet::from([a, b]));
    bridge
        .host_command(HostRequest {
            version: 1,
            command: HostCommand::ApproveEvents {
                recipient,
                grant: events(scope.clone()),
            },
        })
        .unwrap();
    bridge
        .host_command(HostRequest {
            version: 1,
            command: HostCommand::ApproveControls {
                recipient,
                grant: controls(scope, true),
            },
        })
        .unwrap();
    let replacement = permit(&mut bridge, recipient);
    assert_ne!(replacement.id(), approved.id());
    let token = install_test_credential(&replacement, 'b');
    assert!(
        authenticate_hello(
            &endpoint.shared,
            ORIGIN,
            &EventHello {
                version: 1,
                credential: token
            },
            Instant::now()
        )
        .is_ok()
    );
}

#[test]
fn request_ids_poll_ownership_and_protocol_do_not_grant_host_authority() {
    let (mut bridge, endpoint, local, browser, _, _) = setup();
    let approved = permit(&mut bridge, browser);
    let mut output = connect(&mut bridge, &endpoint, &approved);
    command(
        &endpoint,
        &approved,
        1,
        ClientCommand::CreateUser {
            name: "Not allowed".into(),
        },
    );
    bridge.tick();
    assert!(matches!(
        output.try_recv().unwrap(),
        ServerMessage::Response {
            reply: Reply::Error {
                error: Failure {
                    code: FailureCode::Denied,
                    ..
                },
                ..
            },
            ..
        }
    ));
    command(&endpoint, &approved, 1, ClientCommand::Snapshot);
    bridge.tick();
    assert!(matches!(
        output.try_recv().unwrap(),
        ServerMessage::Response {
            reply: Reply::Error {
                error: Failure {
                    code: FailureCode::Invalid,
                    ..
                },
                ..
            },
            ..
        }
    ));
    command(
        &endpoint,
        &approved,
        2,
        ClientCommand::Subscribe { capacity: 8 },
    );
    bridge.tick();
    let ServerMessage::Response {
        reply:
            Reply::Ok {
                response: ClientResponse::Subscribed { subscription, .. },
                ..
            },
        ..
    } = output.try_recv().unwrap()
    else {
        panic!("subscribed");
    };
    command(
        &endpoint,
        &approved,
        3,
        ClientCommand::Poll {
            subscription,
            max_events: 1,
        },
    );
    bridge.tick();
    assert!(matches!(
        output.try_recv().unwrap(),
        ServerMessage::Response {
            reply: Reply::Error {
                error: Failure {
                    code: FailureCode::Unsupported,
                    ..
                },
                ..
            },
            ..
        }
    ));
    assert_eq!(snapshot(&mut bridge, local).users.len(), 2);
}

#[test]
fn stop_fences_commands_and_marks_cleanup_without_waiting_for_network_io() {
    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let approved = permit(&mut bridge, browser);
    let _output = connect(&mut bridge, &endpoint, &approved);
    let queued = command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Input {
            user,
            input: Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        },
    );
    bridge.stop();
    assert_eq!(queued.state.load(Ordering::Acquire), CANCELLED);
    assert!(approved.lease.cleanup_done.load(Ordering::Acquire));
    assert!(endpoint.shared.stop.load(Ordering::Acquire));
    assert!(
        bridge
            .dispatch_local(
                local,
                ClientRequest {
                    version: 1,
                    command: ClientCommand::Snapshot
                }
            )
            .is_err()
    );
    assert!(
        bridge
            .approve_client(browser, ORIGIN, Duration::from_secs(1))
            .is_err()
    );
}

#[test]
fn serialized_event_frames_are_bounded_before_socket_publication() {
    let escaped = "\"".repeat(MAX_RESPONSE_BYTES / 2);
    assert!(encode(&escaped).is_err());
    let message = ServerMessage::Disconnected {
        reason: EndReason::Revoked,
    };
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&encode(&message).unwrap()).unwrap(),
        message
    );
}

#[test]
fn default_off_has_no_permit_and_missing_key_grant_exports_no_key_state_or_events() {
    let bounds = Bounds {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    let mut host = Host::<PlatformWindows>::new(
        71,
        ScreenState {
            revision: 1,
            desktop: bounds,
            screens: vec![bounds],
        },
    )
    .unwrap();
    let receiver = host.register_recipient("unapproved receiver").unwrap();
    let (mut off, _endpoint) = HostBridge::new(host);
    assert!(
        off.approve_client(receiver, ORIGIN, Duration::from_secs(1))
            .is_err()
    );

    let (mut bridge, endpoint, local, browser, user, _) = setup();
    let mut grant = events(UserScope::Only(BTreeSet::from([user])));
    grant.key_events = false;
    bridge
        .host_command(HostRequest {
            version: 1,
            command: HostCommand::ApproveEvents {
                recipient: browser,
                grant,
            },
        })
        .unwrap();
    bridge
        .dispatch_local(
            local,
            ClientRequest {
                version: 1,
                command: ClientCommand::Input {
                    user,
                    input: Input::Key {
                        key: KeyCode(4),
                        transition: Transition::Down,
                    },
                },
            },
        )
        .unwrap();
    let approved = permit(&mut bridge, browser);
    let mut output = connect(&mut bridge, &endpoint, &approved);
    command(
        &endpoint,
        &approved,
        1,
        ClientCommand::Subscribe { capacity: 8 },
    );
    bridge.tick();
    let ServerMessage::Response {
        reply:
            Reply::Ok {
                response: ClientResponse::Subscribed { snapshot, .. },
                ..
            },
        ..
    } = output.try_recv().unwrap()
    else {
        panic!("snapshot");
    };
    assert_eq!(snapshot.users.len(), 1);
    assert!(snapshot.users[0].held_keys.is_none());
    bridge
        .dispatch_local(
            local,
            ClientRequest {
                version: 1,
                command: ClientCommand::Input {
                    user,
                    input: Input::Key {
                        key: KeyCode(4),
                        transition: Transition::Up,
                    },
                },
            },
        )
        .unwrap();
    bridge.tick();
    assert!(output.try_recv().is_err());
    command(
        &endpoint,
        &approved,
        2,
        ClientCommand::Input {
            user,
            input: Input::Key {
                key: KeyCode(4),
                transition: Transition::Down,
            },
        },
    );
    bridge.tick();
    assert!(matches!(
        output.try_recv().unwrap(),
        ServerMessage::Response {
            reply: Reply::Error {
                error: Failure {
                    code: FailureCode::Denied,
                    ..
                },
                ..
            },
            ..
        }
    ));
}

#[test]
fn repeated_receiver_cleanup_retires_only_transport_subscription_slots() {
    use super::network::retire_descriptors;
    let (mut bridge, endpoint, local, first_browser, a, b) = setup();
    let ClientResponse::Subscribed {
        subscription: native,
        ..
    } = bridge
        .dispatch_local(
            local,
            ClientRequest {
                version: 1,
                command: ClientCommand::Subscribe { capacity: 16 },
            },
        )
        .unwrap()
    else {
        panic!("native subscription");
    };
    for index in 0..(MAX_SUBSCRIBERS + 8) {
        let browser = if index == 0 {
            first_browser
        } else {
            let HostResponse::RecipientRegistered { recipient } = bridge
                .host_command(HostRequest {
                    version: 1,
                    command: HostCommand::RegisterRecipient {
                        local_identity: format!("receiver {index}"),
                    },
                })
                .unwrap()
            else {
                panic!("new receiver");
            };
            let scope = UserScope::Only(BTreeSet::from([a, b]));
            bridge
                .host_command(HostRequest {
                    version: 1,
                    command: HostCommand::ApproveEvents {
                        recipient,
                        grant: events(scope.clone()),
                    },
                })
                .unwrap();
            bridge
                .host_command(HostRequest {
                    version: 1,
                    command: HostCommand::ApproveControls {
                        recipient,
                        grant: controls(scope, true),
                    },
                })
                .unwrap();
            recipient
        };
        let approved = permit(&mut bridge, browser);
        let mut output = connect(&mut bridge, &endpoint, &approved);
        command(
            &endpoint,
            &approved,
            1,
            ClientCommand::Subscribe { capacity: 8 },
        );
        bridge.tick();
        assert!(matches!(
            output.try_recv().unwrap(),
            ServerMessage::Response {
                reply: Reply::Ok {
                    response: ClientResponse::Subscribed { .. },
                    ..
                },
                ..
            }
        ));
        match index % 3 {
            0 => bridge.revoke_recipient(browser),
            1 => {
                bridge.tick_at(approved.lease.deadline + Duration::from_millis(1));
            }
            _ => {
                command(&endpoint, &approved, 2, ClientCommand::Disconnect);
                bridge.tick();
                bridge.tick();
            }
        }
        assert!(approved.lease.cleanup_done.load(Ordering::Acquire));
        retire_descriptors(&endpoint.shared).unwrap();
        let state = snapshot(&mut bridge, local);
        assert_eq!(
            state.users.iter().map(|user| user.id).collect::<Vec<_>>(),
            vec![a, b]
        );
        assert!(
            bridge
                .dispatch_local(
                    local,
                    ClientRequest {
                        version: 1,
                        command: ClientCommand::Stats {
                            subscription: native
                        }
                    }
                )
                .is_ok()
        );
    }
    motion(&mut bridge, local, a, 42);
    let ClientResponse::Polled {
        poll: Poll::Events { events },
    } = bridge
        .dispatch_local(
            local,
            ClientRequest {
                version: 1,
                command: ClientCommand::Poll {
                    subscription: native,
                    max_events: 16,
                },
            },
        )
        .unwrap()
    else {
        panic!("native stream remains live");
    };
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, Event::Input { user, .. } if user == a));
}

#[test]
fn reset_and_stop_unsubscribe_transport_tokens_and_leave_native_terminal_ownership_intact() {
    for stop in [false, true] {
        let (mut bridge, endpoint, local, browser, _, _) = setup();
        let ClientResponse::Subscribed {
            subscription: native,
            snapshot: initial,
        } = bridge
            .dispatch_local(
                local,
                ClientRequest {
                    version: 1,
                    command: ClientCommand::Subscribe { capacity: 8 },
                },
            )
            .unwrap()
        else {
            panic!("native stream");
        };
        let approved = permit(&mut bridge, browser);
        let mut output = connect(&mut bridge, &endpoint, &approved);
        command(
            &endpoint,
            &approved,
            1,
            ClientCommand::Subscribe { capacity: 8 },
        );
        bridge.tick();
        let ServerMessage::Response {
            reply:
                Reply::Ok {
                    response:
                        ClientResponse::Subscribed {
                            subscription: remote,
                            ..
                        },
                    ..
                },
            ..
        } = output.try_recv().unwrap()
        else {
            panic!("remote stream");
        };
        let reason = if stop {
            bridge.stop();
            EndReason::SessionEnded
        } else {
            bridge
                .host_command(HostRequest {
                    version: 1,
                    command: HostCommand::Reset {
                        epoch: 72,
                        screens: initial.screens,
                    },
                })
                .unwrap();
            EndReason::Reset
        };
        assert_eq!(
            bridge
                .dispatch_local(
                    browser,
                    ClientRequest {
                        version: 1,
                        command: ClientCommand::Stats {
                            subscription: remote
                        }
                    }
                )
                .unwrap_err()
                .code,
            FailureCode::Missing
        );
        let ClientResponse::Polled { poll } = bridge
            .dispatch_local(
                local,
                ClientRequest {
                    version: 1,
                    command: ClientCommand::Poll {
                        subscription: native,
                        max_events: 1,
                    },
                },
            )
            .unwrap()
        else {
            panic!("native owns its terminal");
        };
        assert_eq!(poll, Poll::Ended { reason });
    }
}

mod connection_path {
    use super::super::network::{ConnectionGuard, drive};
    use super::*;
    use futures_util::{SinkExt, StreamExt, task::AtomicWaker};
    use std::{
        pin::Pin,
        sync::atomic::{AtomicBool, AtomicUsize},
        task::{Context, Poll as TaskPoll},
    };
    use tokio::{
        io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
        time::{sleep, timeout},
    };
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{Message, protocol::Role},
    };

    struct WriteGate {
        remaining: AtomicUsize,
        written: AtomicUsize,
        blocked: AtomicBool,
        dropped: AtomicBool,
        waker: AtomicWaker,
    }
    impl WriteGate {
        fn new() -> Self {
            Self {
                remaining: AtomicUsize::new(usize::MAX),
                written: AtomicUsize::new(0),
                blocked: AtomicBool::new(false),
                dropped: AtomicBool::new(false),
                waker: AtomicWaker::new(),
            }
        }
        fn allow(&self, count: usize) {
            self.remaining.store(count, Ordering::Release);
            self.blocked.store(false, Ordering::Release);
            self.waker.wake();
        }
    }
    struct GatedIo {
        inner: DuplexStream,
        gate: Arc<WriteGate>,
    }
    impl AsyncRead for GatedIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> TaskPoll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, output)
        }
    }
    impl AsyncWrite for GatedIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> TaskPoll<std::io::Result<usize>> {
            self.gate.waker.register(cx.waker());
            let allowance = self.gate.remaining.load(Ordering::Acquire);
            if allowance == 0 {
                self.gate.blocked.store(true, Ordering::Release);
                return TaskPoll::Pending;
            }
            match Pin::new(&mut self.inner).poll_write(cx, &bytes[..bytes.len().min(allowance)]) {
                TaskPoll::Ready(Ok(count)) => {
                    self.gate.written.fetch_add(count, Ordering::AcqRel);
                    self.gate
                        .remaining
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                            Some(remaining.saturating_sub(count))
                        })
                        .unwrap();
                    TaskPoll::Ready(Ok(count))
                }
                other => other,
            }
        }
        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> TaskPoll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> TaskPoll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
    impl Drop for GatedIo {
        fn drop(&mut self) {
            self.gate.dropped.store(true, Ordering::Release);
        }
    }

    // Runs the production post-auth WebSocket driver, actual tungstenite framing,
    // bounded UI channel and actual UI Host application over controlled byte IO.
    // HTTP/token checks are covered independently; this fixture does not mirror
    // admission/send logic and does not bind a socket or invoke private-file helpers.
    async fn start(
        bridge: &mut HostBridge,
        endpoint: &NetworkEndpoint,
        approved: &ClientPermit,
    ) -> (
        WebSocketStream<DuplexStream>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
        Arc<WriteGate>,
    ) {
        let (server, client) = tokio::io::duplex(65_536);
        let gate = Arc::new(WriteGate::new());
        let server = WebSocketStream::from_raw_socket(
            GatedIo {
                inner: server,
                gate: Arc::clone(&gate),
            },
            Role::Server,
            None,
        )
        .await;
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let output = connect(bridge, endpoint, approved);
        let shared = Arc::clone(&endpoint.shared);
        let requests = endpoint.requests.clone();
        let lease = Arc::clone(&approved.lease);
        lease.network_done.store(false, Ordering::Release);
        let task = tokio::spawn(async move {
            let guard = ConnectionGuard {
                lease: Arc::clone(&lease),
            };
            let result = drive(server, shared, requests, lease, output).await;
            drop(guard);
            result
        });
        (client, task, gate)
    }
    fn frame(id: u64, command: ClientCommand) -> Message {
        Message::Text(
            serde_json::to_string(&ClientMessage {
                request_id: id,
                request: ClientRequest {
                    version: 1,
                    command,
                },
            })
            .unwrap()
            .into(),
        )
    }
    async fn next(
        bridge: &mut HostBridge,
        client: &mut WebSocketStream<DuplexStream>,
    ) -> ServerMessage {
        timeout(Duration::from_secs(1), async {
            loop {
                bridge.tick();
                tokio::select! {
                    message = client.next() => {
                        let Message::Text(message) = message.expect("open connection").expect("valid WebSocket") else { panic!("typed server frame"); };
                        return serde_json::from_slice(message.as_bytes()).unwrap();
                    },
                    _ = sleep(Duration::from_millis(1)) => {},
                }
            }
        }).await.expect("connection response deadline")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn motion_button_wheel_release_burst_uses_production_fifo_without_revocation() {
        let (mut bridge, endpoint, local, browser, user, _) = setup();
        let approved = permit(&mut bridge, browser);
        let (mut client, driver, _gate) = start(&mut bridge, &endpoint, &approved).await;
        client
            .send(frame(1, ClientCommand::Subscribe { capacity: 32 }))
            .await
            .unwrap();
        let ServerMessage::Response {
            reply:
                Reply::Ok {
                    response:
                        ClientResponse::Subscribed {
                            snapshot: initial, ..
                        },
                    ..
                },
            ..
        } = next(&mut bridge, &mut client).await
        else {
            panic!("initial snapshot");
        };
        let mut state = ClientState::default();
        state.install_snapshot(initial, 71).unwrap();
        let burst = [
            ClientCommand::InputAtRevision {
                user,
                input: Input::Motion {
                    position: Point { x: 100, y: 200 },
                },
                transform_revision: 1,
            },
            ClientCommand::InputAtRevision {
                user,
                input: Input::Button {
                    button: MouseButton::Primary,
                    transition: Transition::Down,
                },
                transform_revision: 1,
            },
            ClientCommand::InputAtRevision {
                user,
                input: Input::Wheel {
                    axis: WheelAxis::Vertical,
                    units: 120,
                },
                transform_revision: 1,
            },
            ClientCommand::InputAtRevision {
                user,
                input: Input::Wheel {
                    axis: WheelAxis::Horizontal,
                    units: -120,
                },
                transform_revision: 1,
            },
            ClientCommand::InputAtRevision {
                user,
                input: Input::Button {
                    button: MouseButton::Primary,
                    transition: Transition::Up,
                },
                transform_revision: 1,
            },
        ];
        for (offset, command) in burst.into_iter().enumerate() {
            client
                .feed(frame(offset as u64 + 2, command))
                .await
                .unwrap();
        }
        client.flush().await.unwrap(); // No acknowledgement is awaited between inputs.
        let mut responses = Vec::new();
        let mut inputs = 0;
        while responses.len() < 5 || inputs < 5 {
            match next(&mut bridge, &mut client).await {
                ServerMessage::Response {
                    request_id,
                    reply:
                        Reply::Ok {
                            response: ClientResponse::InputAccepted { .. },
                            ..
                        },
                } => responses.push(request_id),
                ServerMessage::EventStream {
                    stream: Poll::Events { events },
                    ..
                } => {
                    inputs += events.len();
                    state.fold_poll(Poll::Events { events }).unwrap();
                }
                other => panic!("unexpected burst result: {other:?}"),
            }
        }
        assert_eq!(responses, vec![2, 3, 4, 5, 6]);
        assert!(approved.lease.live(&endpoint.shared, Instant::now()));
        assert_eq!(
            state.users()[&user].pointer.as_ref().unwrap().position,
            Point { x: 100, y: 200 }
        );
        assert!(
            state.users()[&user]
                .pointer
                .as_ref()
                .unwrap()
                .held_buttons
                .is_empty()
        );
        assert_eq!(snapshot(&mut bridge, local).users.len(), 2);
        client.close(None).await.unwrap();
        drop(client);
        timeout(Duration::from_secs(1), driver)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        bridge.tick();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn partial_application_write_is_never_flushed_after_revoke_or_stop() {
        for stop in [false, true] {
            let (mut bridge, endpoint, local, browser, user, _) = setup();
            let approved = permit(&mut bridge, browser);
            let (mut client, driver, gate) = start(&mut bridge, &endpoint, &approved).await;
            client
                .send(frame(1, ClientCommand::Subscribe { capacity: 16 }))
                .await
                .unwrap();
            assert!(matches!(
                next(&mut bridge, &mut client).await,
                ServerMessage::Response {
                    reply: Reply::Ok {
                        response: ClientResponse::Subscribed { .. },
                        ..
                    },
                    ..
                }
            ));
            gate.allow(16);
            motion(&mut bridge, local, user, 321);
            bridge.tick();
            timeout(Duration::from_secs(1), async {
                while !gate.blocked.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let at_fence = gate.written.load(Ordering::Acquire);
            if stop {
                bridge.stop();
            } else {
                bridge.revoke_recipient(browser);
            }
            // Reopening the byte writer would expose the old terminal-send bug:
            // tungstenite would flush its pending app frame before the terminal.
            gate.allow(usize::MAX);
            assert!(
                timeout(Duration::from_secs(1), driver)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            assert!(gate.dropped.load(Ordering::Acquire));
            assert_eq!(
                gate.written.load(Ordering::Acquire),
                at_fence,
                "no bytes may be flushed after the fence"
            );
            let remaining = timeout(Duration::from_secs(1), client.next())
                .await
                .unwrap();
            assert!(
                !matches!(remaining, Some(Ok(Message::Text(_)))),
                "a partial app frame must not become a valid post-fence message"
            );
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_overflow_fences_all_queued_requests_without_retry_or_application() {
        let (mut bridge, endpoint, local, browser, user, _) = setup();
        let approved = permit(&mut bridge, browser);
        let initial = snapshot(&mut bridge, local);
        let (mut client, driver, _gate) = start(&mut bridge, &endpoint, &approved).await;
        // Deliberately do not tick the UI while filling the real connection FIFO.
        for id in 1..=(MAX_PENDING as u64 + 1) {
            client
                .feed(frame(
                    id,
                    ClientCommand::InputAtRevision {
                        user,
                        input: Input::Motion {
                            position: Point { x: id as i32, y: 0 },
                        },
                        transform_revision: 1,
                    },
                ))
                .await
                .unwrap();
        }
        client.flush().await.unwrap();
        assert!(
            timeout(Duration::from_secs(1), driver)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(!approved.lease.allowed.load(Ordering::Acquire));
        assert_eq!(endpoint.shared.overflows.load(Ordering::Acquire), 1);
        assert_eq!(UI_CAPACITY - endpoint.requests.capacity(), MAX_PENDING);
        bridge.tick();
        assert_eq!(snapshot(&mut bridge, local).users, initial.users);
        assert!(approved.lease.cleanup_done.load(Ordering::Acquire));
    }
}
