// Opt-in external DLL observer. Reuses its parent's exact module/window lifetime.
use super::*;
use mux_application_sdk::{DesktopObservation, DesktopProvenance};

pub(super) fn exercise(api: Api, token: u64, completion: HANDLE, descriptor: &Path, seconds: u64, report: &Report) -> Check {
    let ClientResponse::Subscribed { snapshot, .. } = round_trip(api, token, completion, ClientCommand::Subscribe { capacity: 4096 }, report)? else { return Err("desktop subscription reply".into()); };
    ensure(snapshot.users.len() == 2, "desktop observer requires exactly two authorized users")?;
    let ids = identities(&snapshot.users);
    ensure(ids[0].id != ids[1].id && ids[0].source != ids[1].source, "desktop opaque identity uniqueness")?;
    for user in &snapshot.users { validate_user(user)?; }
    report.record("desktop_baseline", json!({"users": snapshot.users, "screens": snapshot.screens, "epoch": snapshot.epoch, "sequence": snapshot.through_sequence, "synthetic_down": false}))?;
    let model = query(api, token, false)?;
    ensure(model["status"]["pointer_enabled"] == false && model["status"]["key_enabled"] == false, "desktop input defaults off")?;
    // These ordinary C requests must fail locally; broad Core provenance denial
    // has separate deterministic tests. The controller also checks target state.
    request(api, token, ClientCommand::RenameUser { user: ids[0].id, name: "Observer must not rename".into() }, INVALID)?;
    request(api, token, ClientCommand::DisposeUser { user: ids[0].id }, INVALID)?;
    report.record("desktop_observer_mutation_denied", json!({"lifecycle": true, "pending_requests": query(api, token, false)?["status"]["pending_requests"]}))?;
    let finish = descriptor.with_extension("desktop-finish");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut seen = 0usize;
    let mut ended = false;
    loop {
        ensure(Instant::now() < deadline, "desktop observer original deadline; no retry")?;
        for _ in 0..64 {
            let Some(message) = next_message(api, token)? else { break; };
            match message {
                ServerMessage::EventStream { stream: Poll::Events { events }, .. } => {
                    for envelope in events {
                        seen += 1; ensure(seen <= 4096, "desktop observer event bound")?;
                        match &envelope.event {
                            Event::DesktopObserved { user, source, record, .. } => {
                                ensure(ids.iter().any(|id| id.id == *user && id.source == *source), "desktop event leaked another user/source")?;
                                if let DesktopObservation::Key { key, .. } = record.change { key.validate().map_err(|_| "desktop native key schema")?; }
                                report.record("desktop_event", json!({"envelope": envelope}))?;
                            }
                            Event::UserDisposed { user } => { ensure(ids.iter().any(|id| id.id == *user), "unknown desktop disposal")?; report.record("desktop_disposed", json!({"user": user}))?; }
                            _ => return Err("unexpected non-provider event".into()),
                        }
                    }
                }
                ServerMessage::EventStream { stream: Poll::Ended { .. }, .. } => { ended = true; }
                ServerMessage::Disconnected { .. } => { ended = true; }
                _ => return Err("unexpected desktop observer response or native stream gap".into()),
            }
        }
        let model = query(api, token, false)?;
        let current: Vec<UserView> = serde_json::from_value(model["users"].clone()).map_err(|_| "desktop model schema")?;
        for user in &current { validate_user(user)?; ensure(ids.iter().any(|id| id.id == user.id && id.source == user.source), "desktop model leaked identity")?; }
        report.record("desktop_model", json!({"users": &current, "sequence": model["sequence"], "live": model["status"]["live"]}))?;
        let marker = match File::open(&finish) {
            Ok(file) => { let mut bytes = Vec::new(); file.take(17).read_to_end(&mut bytes).map_err(|_| "desktop finish marker read")?; ensure(bytes == b"finish\n" || bytes == b"revoked\n", "desktop finish marker")?; Some(bytes) }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("desktop finish marker read".into()),
        };
        if model["status"]["live"] == false || ended {
            ensure(marker.as_deref() == Some(b"revoked\n"), "unplanned desktop terminal")?;
            ensure(matches!(model["status"]["terminal"].as_str(), Some("remote_end" | "transport")), "unexpected desktop terminal reason")?;
            ensure(current.is_empty(), "desktop terminal retained users/holds")?;
            report.record("desktop_revoked_terminal", json!({"cleared": true}))?;
            break;
        }
        if marker.as_deref() == Some(b"finish\n") { break; }
        pump_wait(completion, (Instant::now() + Duration::from_millis(100)).min(deadline))?;
    }
    report.record("desktop_observer_complete", json!({"native_events": seen, "exact_two_initial_users": true, "no_foreign_user": true, "local_input_disabled": true}))
}

fn validate_user(user: &UserView) -> Check {
    let desktop = user.desktop.as_ref().ok_or("desktop metadata absent")?;
    ensure(desktop.version == 1 && desktop.provenance == DesktopProvenance::VirtualDesktopObservation, "desktop provenance/version")?;
    ensure(user.cursor.is_none() && user.held_keys.is_none(), "native observation overloaded cooperative state")?;
    if let Some(keys) = &desktop.native_held_keys {
        ensure(keys.len() <= 247, "desktop native key bound")?;
        for key in keys { key.validate().map_err(|_| "desktop native held key")?; }
    }
    Ok(())
}
