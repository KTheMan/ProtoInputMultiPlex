//! Tests the actual existing GUI authority projection, not event receiver grants.
use super::*;

fn admitted() -> (AppHandle, Reply) {
    let (app, _requests) = app_channel();
    let mut reply = Reply {
        acknowledged: true,
        client: 9,
        clients: vec![9],
        authority: Some(agent_runtime::WorkerAuthority {
            policy_generation: 0,
            active_user_ids: Vec::new(),
        }),
        ..Reply::default()
    };
    reply.policy.enabled = true;
    {
        let mut state = app.recovery.write().unwrap();
        state.policy = reply.policy.clone();
        state.connections.insert(
            9,
            ConnectionAuthority {
                certified: BTreeSet::new(),
            },
        );
    }
    (app, reply)
}

#[test]
fn fresh_gui_publication_cannot_refresh_stale_transport_counters() {
    let (app, reply) = admitted();
    let stale = crate::gui_application_events::PublicStatus {
        phase: "active".into(),
        session_active: true,
        transport_sample_age_ms: Some(3_000),
        connected_clients: Some(7),
        cleanup_pending: Some(4),
        uncertain_admissions: Some(2),
        queue_overflows: Some(1),
        ..Default::default()
    };
    for sequence in 1..=2 {
        // The GUI can continue repainting while its lifecycle actor is in IO.
        app.publish_application_events(stale.clone());
        let observed = app
            .public_reply(reply.clone(), 9, sequence, true)
            .unwrap()
            .application_events
            .unwrap();
        assert!(observed.transport_sample_age_ms.unwrap() >= 3_000);
        assert!(observed.connected_clients.is_none());
        assert!(observed.cleanup_pending.is_none());
        assert!(observed.uncertain_admissions.is_none());
        assert!(observed.queue_overflows.is_none());
        assert!(
            !serde_json::to_value(observed)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("connected_clients")
        );
    }
}

#[test]
fn application_event_summary_requires_live_admitted_gui_authority_and_fresh_sample() {
    let (app, reply) = admitted();
    app.publish_application_events(crate::gui_application_events::PublicStatus {
        phase: "local".into(),
        session_active: true,
        local_virtual_users: 2,
        ..Default::default()
    });
    let projected = app.public_reply(reply.clone(), 9, 1, true).unwrap();
    assert_eq!(projected.application_events.unwrap().local_virtual_users, 2);
    assert!(app.public_reply(reply.clone(), 10, 1, true).is_err());
    app.application_events.write().unwrap().as_mut().unwrap().1 =
        Instant::now() - Duration::from_secs(3);
    assert!(
        app.public_reply(reply.clone(), 9, 2, true)
            .unwrap()
            .application_events
            .is_none()
    );
    app.publish_application_events(crate::gui_application_events::PublicStatus::default());
    app.begin_stop();
    let terminal = app.public_reply(reply.clone(), 9, 3, true).unwrap();
    assert!(terminal.terminal && terminal.application_events.is_none());
    app.revoke();
    assert!(app.public_reply(reply, 9, 4, true).is_err());
}
