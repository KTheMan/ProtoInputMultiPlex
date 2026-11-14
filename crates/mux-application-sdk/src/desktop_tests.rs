use crate::*;
use mux_core::desktop::{Bounds, CursorShape, MouseButton};
use std::collections::BTreeSet;

fn screens() -> ScreenState {
    let desktop = Bounds {
        left: 0,
        top: 0,
        right: 640,
        bottom: 480,
    };
    ScreenState {
        revision: 1,
        desktop,
        screens: vec![desktop],
    }
}
fn baseline() -> DesktopBaselineV1 {
    DesktopBaselineV1 {
        version: 1,
        next_sequence: 1,
        screens: screens(),
        position: Point { x: 4, y: 5 },
        cursor: ConfiguredDesktopCursor {
            shape: CursorShape::System,
            size: 32.5,
            color: [1, 2, 3],
            visible: true,
            follow_application: false,
        },
        held_buttons: vec![MouseButton::X2],
        native_held_keys: vec![WindowsVirtualKey {
            virtual_key: 0xA3,
            scan_code: 0x1D,
            prefix: ScanPrefix::E0,
        }],
    }
}
fn setup() -> (Host, RecipientId, RecipientId, UserId) {
    let mut host: Host = Host::new(71, screens()).unwrap();
    let owner = host.register_recipient("private provider").unwrap();
    let receiver = host.register_recipient("SDK receiver").unwrap();
    let user = host
        .create_desktop_observed(owner, "Observed", baseline())
        .unwrap();
    for recipient in [owner, receiver] {
        host.approve_events(
            recipient,
            Grant {
                users: UserScope::Only(BTreeSet::from([user])),
                pointer_events: true,
                key_events: true,
                ..Default::default()
            },
        )
        .unwrap();
        host.approve_desktop_observation(
            recipient,
            Some(DesktopConsentV1 {
                version: 1,
                text_units: true,
            }),
        )
        .unwrap();
        host.negotiate_desktop_observation(recipient, Some(1))
            .unwrap();
    }
    (host, owner, receiver, user)
}
fn request(command: ClientCommand) -> ClientRequest {
    ClientRequest {
        version: 1,
        command,
    }
}
fn subscribe(host: &mut Host, receiver: RecipientId) -> (SubscriptionId, Snapshot) {
    match host
        .dispatch(receiver, request(ClientCommand::Subscribe { capacity: 16 }))
        .unwrap()
    {
        ClientResponse::Subscribed {
            subscription,
            snapshot,
        } => (subscription, snapshot),
        other => panic!("expected subscription, got {other:?}"),
    }
}
fn poll(host: &mut Host, receiver: RecipientId, subscription: SubscriptionId) -> Poll {
    match host
        .dispatch(
            receiver,
            request(ClientCommand::Poll {
                subscription,
                max_events: 16,
            }),
        )
        .unwrap()
    {
        ClientResponse::Polled { poll } => poll,
        other => panic!("expected poll, got {other:?}"),
    }
}

#[test]
fn desktop_wire_optin_is_additive_and_legacy_hello_stays_strict() {
    let (mut host, _, receiver, _) = setup();
    host.negotiate_desktop_observation(receiver, None).unwrap();
    let legacy = serde_json::to_value(host.ready_for(receiver).unwrap()).unwrap();
    assert!(legacy.get("desktop").is_none());
    let strict =
        serde_json::json!({"version":1,"credential":"private","desktop_observation_version":1});
    assert!(serde_json::from_value::<EventHello>(strict.clone()).is_err());
    let opted: DesktopEventHello = serde_json::from_value(strict).unwrap();
    assert_eq!(opted.desktop_observation_version, 1);
    let (_, legacy_snapshot) = subscribe(&mut host, receiver);
    assert!(legacy_snapshot.users.is_empty());
    host.negotiate_desktop_observation(receiver, Some(1))
        .unwrap();
    assert_eq!(
        host.ready_for(receiver).unwrap().desktop.unwrap().version,
        1
    );
}

#[test]
fn desktop_client_requires_optin_and_clears_held_state_on_dispose_or_gap() {
    let (mut host, owner, receiver, user) = setup();
    host.approve_events(
        receiver,
        Grant {
            users: UserScope::Only(BTreeSet::from([user])),
            pointer_events: false,
            key_events: true,
            ..Default::default()
        },
    )
    .unwrap();
    let (subscription, snapshot) = subscribe(&mut host, receiver);
    assert!(snapshot.users[0].pointer.is_none());
    let mut legacy = ClientState::default();
    assert!(legacy.install_snapshot(snapshot.clone(), 71).is_err());
    let mut model = ClientState::default();
    model.enable_desktop_observation(1).unwrap();
    model.install_snapshot(snapshot, 71).unwrap();
    assert_eq!(
        model
            .users()
            .values()
            .next()
            .unwrap()
            .desktop
            .as_ref()
            .unwrap()
            .native_held_keys
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    host.publish_desktop_observation(
        owner,
        user,
        DesktopRecordV1 {
            version: 1,
            sequence: 1,
            screen_revision: 1,
            admission: DesktopAdmission::LogicalOnly,
            change: DesktopObservation::Motion {
                position: Point { x: 7, y: 8 },
            },
        },
    )
    .unwrap();
    host.publish_desktop_observation(
        owner,
        user,
        DesktopRecordV1 {
            version: 1,
            sequence: 2,
            screen_revision: 1,
            admission: DesktopAdmission::NativeSubmissionAccepted,
            change: DesktopObservation::Key {
                key: baseline().native_held_keys[0],
                transition: Transition::Up,
            },
        },
    )
    .unwrap();
    model
        .fold_poll(poll(&mut host, receiver, subscription))
        .unwrap();
    assert!(
        model
            .users()
            .values()
            .next()
            .unwrap()
            .desktop
            .as_ref()
            .unwrap()
            .native_held_keys
            .as_ref()
            .unwrap()
            .is_empty()
    );
    host.retire_desktop_observed(owner, user).unwrap();
    model
        .fold_poll(poll(&mut host, receiver, subscription))
        .unwrap();
    assert_eq!(model.users().len(), 0);
    let (mut host, _, receiver, _) = setup();
    let (_, snapshot) = subscribe(&mut host, receiver);
    let mut model = ClientState::default();
    model.enable_desktop_observation(1).unwrap();
    model.install_snapshot(snapshot.clone(), 71).unwrap();
    model
        .fold_poll(Poll::Gap {
            after_sequence: snapshot.through_sequence,
            lost_through: snapshot.through_sequence + 1,
        })
        .unwrap();
    assert_eq!(model.users().len(), 0);
    assert_eq!(model.state(), &StreamState::NeedsSnapshot);
}

#[test]
fn desktop_owner_and_delegation_never_allow_mutation() {
    let (mut host, owner, receiver, user) = setup();
    for recipient in [owner, receiver] {
        for users in [UserScope::Only(BTreeSet::from([user])), UserScope::AllOwned] {
            let control = ControlGrant {
                users,
                user_lifecycle: true,
                cursor: true,
                pointer_input: true,
                key_input: true,
                delegated_users: true,
                ..Default::default()
            };
            // AllOwned on a different recipient selects nothing, but must still never authorize this user.
            let _ = host.approve_controls(recipient, control);
            for command in [
                ClientCommand::RenameUser {
                    user,
                    name: "Changed".into(),
                },
                ClientCommand::DisposeUser { user },
                ClientCommand::Cursor {
                    user,
                    control: CursorControl::Visible(false),
                },
                ClientCommand::Input {
                    user,
                    input: Input::Key {
                        key: KeyCode(4),
                        transition: Transition::Down,
                    },
                },
            ] {
                assert_eq!(
                    host.dispatch(recipient, request(command)).unwrap_err().code,
                    FailureCode::Denied
                );
            }
        }
    }
    assert!(host.is_desktop_observed(user));
}
