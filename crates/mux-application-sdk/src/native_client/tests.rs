//! Authored only. Mock sockets do not open a port, window, or input provider.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::AtomicUsize;
use tokio::{
    io::{AsyncReadExt, duplex},
    sync::oneshot,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
fn request(command: ClientCommand) -> ClientRequest {
    ClientRequest {
        version: PROTOCOL_VERSION,
        command,
    }
}

#[test]
fn actual_engine_gap_updates_and_queued_old_token_frames_are_fenced_during_refresh() {
    let (mut client, _, _) = mock_client();
    let (mut host, native, ready, users) = fixture();
    client.fold(&ServerMessage::Ready { ready }).unwrap();
    let subscribe = ClientCommand::Subscribe { capacity: 1 };
    let id = client.request(request(subscribe.clone())).unwrap();
    let response = host.dispatch(native, request(subscribe)).unwrap();
    client
        .fold(&ServerMessage::Response {
            request_id: id,
            reply: Reply::Ok {
                version: PROTOCOL_VERSION,
                response,
            },
        })
        .unwrap();
    let old = client.subscription.unwrap();
    let publish = |host: &mut Host, x| {
        host.dispatch(
            native,
            request(ClientCommand::Input {
                user: users[0],
                input: Input::Motion {
                    position: Point { x, y: 0 },
                },
            }),
        )
        .unwrap();
    };
    let poll = |host: &mut Host, token| {
        let ClientResponse::Polled { poll } = host
            .dispatch(
                native,
                request(ClientCommand::Poll {
                    subscription: token,
                    max_events: 128,
                }),
            )
            .unwrap()
        else {
            panic!("poll");
        };
        ServerMessage::EventStream {
            subscription: token,
            stream: poll,
        }
    };
    publish(&mut host, 1);
    publish(&mut host, 2);
    client.fold(&poll(&mut host, old)).unwrap();
    let first_floor = client.model.last_sequence();
    publish(&mut host, 3);
    client.fold(&poll(&mut host, old)).unwrap();
    assert!(client.model.last_sequence() > first_floor);
    assert_eq!(client.terminal, None);
    let refresh = ClientCommand::Resubscribe {
        subscription: old,
        capacity: 1,
    };
    let refresh_id = client.request(request(refresh.clone())).unwrap();
    // These old-token frames were already queued before the refresh response.
    publish(&mut host, 4);
    assert!(client.shared.enqueue(poll(&mut host, old), 100));
    assert!(client.shared.enqueue(
        ServerMessage::EventStream {
            subscription: old,
            stream: Poll::Events { events: vec![] }
        },
        100
    ));
    let response = host.dispatch(native, request(refresh)).unwrap();
    let ClientResponse::Subscribed {
        subscription: new,
        snapshot,
    } = &response
    else {
        panic!("refresh");
    };
    let through = snapshot.through_sequence;
    let new = *new;
    assert_ne!(new, old);
    assert!(client.shared.enqueue(
        ServerMessage::Response {
            request_id: refresh_id,
            reply: Reply::Ok {
                version: PROTOCOL_VERSION,
                response
            }
        },
        100
    ));
    publish(&mut host, 5);
    assert!(client.shared.enqueue(poll(&mut host, new), 100));
    client.drain().unwrap();
    assert_eq!(client.model.state(), &StreamState::NeedsSnapshot);
    assert!(client.model.users().is_empty());
    assert_eq!(client.model.last_sequence(), through);
    client.drain().unwrap();
    assert_eq!(client.model.state(), &StreamState::NeedsSnapshot);
    client.drain().unwrap();
    assert_eq!(client.model.state(), &StreamState::Live);
    assert_eq!(client.subscription, Some(new));
    client.drain().unwrap();
    assert_eq!(
        client.model.users()[&users[0]]
            .pointer
            .as_ref()
            .unwrap()
            .position,
        Point { x: 5, y: 0 }
    );
    assert!(client.pending.is_empty());
    assert!(client.drain().unwrap().is_none());
    assert_eq!(
        client.fold(&ServerMessage::EventStream {
            subscription: old,
            stream: Poll::Events { events: vec![] }
        }),
        Err(ClientError::Protocol)
    );
}
fn config() -> ConnectConfig {
    ConnectConfig {
        endpoint: "ws://127.0.0.1:12345/application-events/v1".into(),
        exact_origin: "http://localhost:8080".into(),
        event_credential: "a".repeat(64),
    }
}
pub(super) fn mock_client() -> (NativeClient, (), Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let binding = wake::Binding::mock(Arc::clone(&counter));
    let shared = Arc::new(Shared {
        mailbox: Mutex::new(Mailbox {
            outgoing: VecDeque::new(),
            urgent: VecDeque::new(),
            incoming: VecDeque::new(),
            bytes: 0,
            signalled: false,
            terminal: None,
        }),
        stop: AtomicBool::new(false),
        changed: Notify::new(),
        requests_changed: Notify::new(),
        wake: binding.target.clone(),
    });
    (
        NativeClient {
            desktop_version: None,
            shared,
            worker: None,
            binding,
            model: EventClientState::default(),
            hosted_receipt: crate::client::HostedReceipt::default(),
            ready: None,
            subscription: None,
            pending: VecDeque::new(),
            owner_pending: BTreeMap::new(),
            seen_apply: BTreeSet::new(),
            owned: None,
            next_id: 1,
            next_ack_id: 1,
            pointer: false,
            keys: false,
            terminal: None,
            uncertain: Vec::new(),
            _local: PhantomData,
        },
        (),
        counter,
    )
}
fn fixture() -> (Host, RecipientId, EventReady, Vec<UserId>) {
    let bounds = mux_core::desktop::Bounds {
        left: 0,
        top: 0,
        right: 800,
        bottom: 600,
    };
    let mut host = Host::new(
        91,
        ScreenState {
            revision: 1,
            desktop: bounds,
            screens: vec![bounds],
        },
    )
    .unwrap();
    let owner = host.register_recipient("trusted-local-owner").unwrap();
    let native = host.register_recipient("external-native").unwrap();
    let users = vec![
        host.create_owned_user(owner, "One").unwrap(),
        host.create_owned_user(owner, "Two").unwrap(),
    ];
    // Delegated input still requires the owning recipient's master consent.
    // Remote Only(users) grants below remain separate from that owner consent.
    host.approve_events(
        owner,
        Grant {
            users: UserScope::AllOwned,
            pointer_events: true,
            key_events: true,
            owned_windows: false,
            host_state: false,
        },
    )
    .unwrap();
    host.approve_controls(
        owner,
        ControlGrant {
            users: UserScope::AllOwned,
            pointer_input: true,
            key_input: true,
            ..Default::default()
        },
    )
    .unwrap();
    let scope = UserScope::Only(users.iter().copied().collect());
    host.approve_events(
        native,
        Grant {
            users: scope.clone(),
            pointer_events: true,
            key_events: true,
            owned_windows: false,
            host_state: false,
        },
    )
    .unwrap();
    host.approve_controls(
        native,
        ControlGrant {
            users: scope,
            delegated_users: true,
            pointer_input: true,
            key_input: true,
            ..Default::default()
        },
    )
    .unwrap();
    let ready = host.ready_for(native).unwrap();
    (host, native, ready, users)
}
fn install(client: &mut NativeClient, host: &mut Host, recipient: RecipientId, ready: EventReady) {
    client.fold(&ServerMessage::Ready { ready }).unwrap();
    let id = client
        .request(request(ClientCommand::Subscribe { capacity: 16 }))
        .unwrap();
    let response = host
        .dispatch(
            recipient,
            request(ClientCommand::Subscribe { capacity: 16 }),
        )
        .unwrap();
    client
        .fold(&ServerMessage::Response {
            request_id: id,
            reply: Reply::Ok {
                version: PROTOCOL_VERSION,
                response,
            },
        })
        .unwrap();
}
#[test]
fn setup_rejects_remote_paths_credentials_in_url_and_nonexact_origins() {
    assert!(network::validate_config(&config()).is_ok());
    for endpoint in [
        "ws://localhost:12345/application-events/v1",
        "ws://127.0.0.1:12345/application-events/v1?token=x",
        "ws://127.0.0.1:0123/application-events/v1",
        "ws://127.0.0.2:12345/application-events/v1",
    ] {
        let mut c = config();
        c.endpoint = endpoint.into();
        assert_eq!(network::validate_config(&c), Err(ClientError::Invalid));
    }
    for origin in [
        "null",
        "*",
        "http://localhost:8080/",
        "http://u@localhost",
        "file://localhost",
        "http://localhost/#x",
    ] {
        let mut c = config();
        c.exact_origin = origin.into();
        assert_eq!(network::validate_config(&c), Err(ClientError::Invalid));
    }
    let c = config();
    assert!(!format!("{c:?}").contains(&c.event_credential));
}
#[test]
fn default_off_families_and_separate_control_grant_cannot_escalate() {
    let (mut client, _rx, _) = mock_client();
    let (mut host, native, ready, users) = fixture();
    install(&mut client, &mut host, native, ready);
    let motion = request(ClientCommand::Input {
        user: users[0],
        input: Input::Motion {
            position: Point { x: 10, y: 20 },
        },
    });
    assert_eq!(client.request(motion.clone()), Err(ClientError::Denied));
    client.set_input_enabled(true, false).unwrap();
    assert!(client.request(motion.clone()).is_ok());
    client.ready.as_mut().unwrap().controls.pointer_input = false;
    assert_eq!(client.request(motion.clone()), Err(ClientError::Denied));
    client.ready.as_mut().unwrap().controls.pointer_input = true;
    client.ready.as_mut().unwrap().controls.users = UserScope::AllOwned;
    assert_eq!(client.request(motion.clone()), Err(ClientError::Denied));
    client.ready.as_mut().unwrap().controls.users =
        UserScope::Only(users.iter().copied().collect());
    client.ready.as_mut().unwrap().events.users = UserScope::AllOwned;
    assert_eq!(client.request(motion), Err(ClientError::Denied));
    let key = Input::Key {
        key: KeyCode(0x04),
        transition: Transition::Down,
    };
    assert_eq!(
        client.request(request(ClientCommand::Input {
            user: users[1],
            input: key
        })),
        Err(ClientError::Denied)
    );
    for command in [
        ClientCommand::Snapshot,
        ClientCommand::CreateUser {
            name: "Unauthorized third pool".into(),
        },
    ] {
        assert_eq!(client.request(request(command)), Err(ClientError::Denied));
    }
    assert!(
        serde_json::from_str::<ClientRequest>(
            r#"{"version":1,"command":{"kind":"approve_events"}}"#
        )
        .is_err()
    );
    client.set_input_enabled(false, false).unwrap();
    assert!(client.model.users().is_empty());
    assert!(client.query().status.remote_cleanup_unconfirmed);
}
#[test]
fn queued_arrival_coalesces_ack_rearms_and_overflow_discards_state() {
    let (mut client, _rx, counter) = mock_client();
    let (_, _, ready, _) = fixture();
    for _ in 0..2 {
        assert!(client.shared.enqueue(
            ServerMessage::Ready {
                ready: ready.clone()
            },
            1
        ));
    }
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    assert!(client.drain().unwrap().is_some());
    assert_eq!(counter.load(Ordering::Relaxed), 2);
    // A duplicate Ready is invalid, so it cannot partly mutate the live model.
    assert_eq!(client.drain(), Err(ClientError::Protocol));
    assert!(client.model.users().is_empty());
    let (mut client, _rx, _) = mock_client();
    for _ in 0..MAX_INCOMING {
        assert!(client.shared.enqueue(
            ServerMessage::Ready {
                ready: ready.clone()
            },
            1
        ));
    }
    assert!(!client.shared.enqueue(ServerMessage::Ready { ready }, 1));
    assert!(client.drain().unwrap().is_none());
    assert_eq!(client.terminal, Some(StopReason::QueueOverflow));
    assert!(client.shared.mailbox.lock().unwrap().incoming.is_empty());
}
#[test]
fn gap_clears_holds_and_requires_explicit_current_token_resubscribe() {
    let (mut client, _rx, _) = mock_client();
    let (mut host, native, ready, _) = fixture();
    install(&mut client, &mut host, native, ready);
    let token = client.subscription.unwrap();
    let seq = client.model.last_sequence();
    client
        .fold(&ServerMessage::EventStream {
            subscription: token,
            stream: Poll::Gap {
                after_sequence: seq,
                lost_through: seq + 3,
            },
        })
        .unwrap();
    assert!(client.model.users().is_empty());
    assert_eq!(client.model.state(), &StreamState::NeedsSnapshot);
    assert_eq!(
        client.request(request(ClientCommand::Subscribe { capacity: 16 })),
        Err(ClientError::Denied)
    );
    assert!(
        client
            .request(request(ClientCommand::Resubscribe {
                subscription: token,
                capacity: 16
            }))
            .is_ok()
    );
    let pending = client.pending.front().unwrap().0;
    assert_eq!(
        client.fold(&ServerMessage::Response {
            request_id: pending + 1,
            reply: Reply::Ok {
                version: PROTOCOL_VERSION,
                response: ClientResponse::Acknowledged
            }
        }),
        Err(ClientError::Protocol)
    );
    assert_eq!(client.pending.front().unwrap().0, pending);
}
#[tokio::test(flavor = "current_thread")]
async fn socket_driver_burst_motion_button_key_wheel_roundtrips_same_two_server_users() {
    let (mut client, _requests, _) = mock_client();
    let shared = Arc::clone(&client.shared);
    let (mut host, native, ready, users) = fixture();
    let (left, right) = duplex(8192);
    let socket = WebSocketStream::from_raw_socket(left, Role::Client, None).await;
    let mut server = WebSocketStream::from_raw_socket(right, Role::Server, None).await;
    let (done, finished) = oneshot::channel::<()>();
    let ids = users.clone();
    let server_task = async move {
        server
            .send(Message::Text(
                serde_json::to_string(&ServerMessage::Ready { ready })
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        let mut token = None;
        for expected in 1..=5 {
            let frame = server.next().await.unwrap().unwrap();
            let message: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(message.request_id, expected);
            let response = host.dispatch(native, message.request).unwrap();
            if let ClientResponse::Subscribed { subscription, .. } = &response {
                token = Some(*subscription);
            }
            server
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::Response {
                        request_id: expected,
                        reply: Reply::Ok {
                            version: PROTOCOL_VERSION,
                            response,
                        },
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }
        let response = host
            .dispatch(
                native,
                request(ClientCommand::Poll {
                    subscription: token.unwrap(),
                    max_events: 128,
                }),
            )
            .unwrap();
        let ClientResponse::Polled { poll } = response else {
            panic!("poll");
        };
        server
            .send(Message::Text(
                serde_json::to_string(&ServerMessage::EventStream {
                    subscription: token.unwrap(),
                    stream: poll,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let _ = finished.await;
    };
    let ui = async move {
        loop {
            if matches!(client.drain().unwrap(), Some(ServerMessage::Ready { .. })) {
                break;
            }
            tokio::task::yield_now().await;
        }
        client
            .request(request(ClientCommand::Subscribe { capacity: 16 }))
            .unwrap();
        while client.subscription.is_none() {
            client.drain().unwrap();
            tokio::task::yield_now().await;
        }
        assert_eq!(
            client.model.users().keys().copied().collect::<Vec<_>>(),
            ids
        );
        client.set_input_enabled(true, true).unwrap();
        assert_eq!(
            client
                .request(request(ClientCommand::Input {
                    user: ids[0],
                    input: Input::Motion {
                        position: Point { x: 45, y: 67 }
                    }
                }))
                .unwrap(),
            2
        );
        assert_eq!(
            client
                .request(request(ClientCommand::Input {
                    user: ids[0],
                    input: Input::Button {
                        button: MouseButton::Primary,
                        transition: Transition::Down
                    }
                }))
                .unwrap(),
            3
        );
        assert_eq!(
            client
                .request(request(ClientCommand::Input {
                    user: ids[0],
                    input: Input::Key {
                        key: KeyCode(0x04),
                        transition: Transition::Down
                    }
                }))
                .unwrap(),
            4
        );
        assert_eq!(
            client
                .request(request(ClientCommand::Input {
                    user: ids[1],
                    input: Input::Wheel {
                        axis: WheelAxis::Horizontal,
                        units: 120
                    }
                }))
                .unwrap(),
            5
        );
        loop {
            if matches!(
                client.drain().unwrap(),
                Some(ServerMessage::EventStream { .. })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let first = client.model.users()[&ids[0]].pointer.as_ref().unwrap();
        assert_eq!(first.position, Point { x: 45, y: 67 });
        assert!(first.held_buttons.contains(&MouseButton::Primary));
        assert!(
            client.model.users()[&ids[1]]
                .pointer
                .as_ref()
                .unwrap()
                .held_buttons
                .is_empty()
        );
        assert_eq!(
            client.model.users()[&ids[0]].held_keys.as_ref().unwrap(),
            &vec![KeyCode(0x04)]
        );
        assert!(
            client.model.users()[&ids[1]]
                .held_keys
                .as_ref()
                .unwrap()
                .is_empty()
        );
        assert!(client.pending.is_empty());
        client.stop();
        assert!(client.model.users().is_empty());
        let _ = done.send(());
    };
    let (result, _, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            network::drive(
                socket,
                &shared,
                tokio::time::Instant::now() + std::time::Duration::from_secs(5)
            ),
            server_task,
            ui
        )
    })
    .await
    .expect("bounded mock connection");
    assert_eq!(result, Err(StopReason::LocalStop));
}
#[tokio::test(flavor = "current_thread")]
async fn interrupted_dirty_send_drops_without_close_or_later_payload_flush() {
    let (client, _requests, _) = mock_client();
    let shared = Arc::clone(&client.shared);
    let (left, mut peer) = duplex(1);
    let mut socket = WebSocketStream::from_raw_socket(left, Role::Client, None).await;
    let sender_shared = Arc::clone(&shared);
    let writer = async move {
        let result = network::send(&mut socket, &sender_shared, "x".repeat(65536)).await;
        drop(socket);
        result
    };
    let stopper = async move {
        let mut prefix = [0u8; 1];
        peer.read_exact(&mut prefix).await.unwrap();
        shared.finish(StopReason::LocalStop);
        let mut remaining = Vec::new();
        peer.read_to_end(&mut remaining).await.unwrap();
        // At most one already-buffered byte exists after the stop fence. No Close
        // frame or queued application remainder may be flushed by cleanup.
        assert!(remaining.len() <= 1);
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(writer, stopper)
    })
    .await
    .expect("bounded stop");
    assert_eq!(result, Err(StopReason::LocalStop));
}
#[test]
fn generated_header_native_exports_are_feature_selected_and_host_abi_stays_stable() {
    let plain = crate::abi_contract::render_header_for(false);
    let native = crate::abi_contract::render_header_for(true);
    assert!(plain.contains("pim_app_host_create"));
    for function in crate::abi_contract::NATIVE_FUNCTIONS {
        assert_eq!(plain.matches(function.name).count(), 0);
        assert_eq!(native.matches(function.name).count(), 1);
    }
    let _: unsafe extern "C" fn(usize, *const u8, usize, *mut u64, *mut u32) -> i32 =
        ffi::pim_app_native_connect;
    let _: unsafe extern "C" fn(u64, *const u8, usize, *mut u64) -> i32 =
        ffi::pim_app_native_request;
    let _: extern "C" fn(u64, u32, u32) -> i32 = ffi::pim_app_native_enable_input;
    let _: unsafe extern "C" fn(u64, usize, u32, *mut u64) -> i32 =
        ffi::pim_app_native_window_register;
    let _: unsafe extern "C" fn(u64, u64, u32, *mut u64) -> i32 =
        ffi::pim_app_native_window_refresh;
    let _: unsafe extern "C" fn(u64, u64, u32, *mut u64) -> i32 = ffi::pim_app_native_window_notify;
    let _: unsafe extern "C" fn(u64, u64, *mut u64) -> i32 = ffi::pim_app_native_window_retire;
    let _: extern "C" fn(u64) -> i32 = ffi::pim_app_native_drain;
    let _: extern "C" fn(u64) -> i32 = ffi::pim_app_native_query;
    let _: unsafe extern "C" fn(u64, *const u8, usize) -> i32 = ffi::pim_app_native_local_toolkit;
    let _: unsafe extern "C" fn(u64, *mut usize) -> i32 = ffi::pim_app_native_response_size;
    let _: unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32 =
        ffi::pim_app_native_response_read;
    let _: extern "C" fn(u64) -> i32 = ffi::pim_app_native_response_discard;
    let _: extern "C" fn(u64) -> i32 = ffi::pim_app_native_stop;
    let _: extern "C" fn(u64) -> i32 = ffi::pim_app_native_destroy;
    let _: unsafe extern "C" fn(u64, *mut usize) -> i32 = ffi::pim_app_native_completion_handle;
    assert!(native.contains("PIM_APP_NATIVE_CLIENT_ABI_VERSION 3u"));
}

#[test]
fn owner_apply_is_queued_only_by_ui_drain_and_duplicate_is_fenced() {
    let (mut client, _, _) = mock_client();
    let (_, _, ready, _) = fixture();
    client.fold(&ServerMessage::Ready { ready }).unwrap();
    let window: WindowId = serde_json::from_str("1").unwrap();
    let apply = ServerMessage::WindowApply {
        operation_id: 7,
        window,
        token: 11,
        generation: 1,
        control: WindowControl::Hide,
    };
    client.fold(&apply).unwrap();
    let queued = client
        .shared
        .mailbox
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .urgent
        .front()
        .cloned()
        .unwrap();
    assert_eq!(queued.request_id, (1_u64 << 63) | 1);
    assert!(matches!(
        queued.operation,
        OwnerOperation::ApplyResult {
            operation_id: 7,
            result: Reply::Error {
                error: Failure {
                    code: FailureCode::Uncertain,
                    ..
                },
                ..
            },
            ..
        }
    ));
    assert_eq!(client.fold(&apply), Err(ClientError::Protocol));
    client.stop();
    assert!(client.uncertain.contains(&((1_u64 << 63) | 1)));
}

#[test]
fn request_admission_is_bounded_and_stopped_requests_never_replay() {
    let (mut client, _rx, _) = mock_client();
    let (mut host, native, ready, users) = fixture();
    install(&mut client, &mut host, native, ready);
    client.set_input_enabled(true, false).unwrap();
    let command = request(ClientCommand::Input {
        user: users[0],
        input: Input::Motion {
            position: Point { x: 5, y: 5 },
        },
    });
    // The mock outgoing queue still holds the initial Subscribe, so queue
    // backpressure can arrive before the parallel pending-request bound.
    let mut admitted = 0;
    while client.request(command.clone()).is_ok() {
        admitted += 1;
        assert!(admitted <= MAX_PENDING);
    }
    assert!(admitted > 0);
    client.stop();
    assert_eq!(client.uncertain.len(), admitted);
    assert_eq!(client.request(command), Err(ClientError::Closed));
    assert!(client.model.users().is_empty());
}
