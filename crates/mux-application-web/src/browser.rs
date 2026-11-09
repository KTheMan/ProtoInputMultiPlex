use crate::event_hooks::{
    ClickOptions, ClickPairs, EventFilter, Registry, hosted_deadline_pending,
    validate_snapshot_query,
};
use crate::geometry::CanvasGeometry;
use crate::local_input::{LocalInput, hid_key, mouse_button, selects};
use mux_application_sdk::*;
use mux_core::application_event::Event as ApplicationEvent;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::{Rc, Weak};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::Event;
use web_sys::*;

const MAX_PENDING: usize = 32;
const MAX_BUFFERED: u32 = 256 * 1024;

struct Listener {
    target: EventTarget,
    name: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}
impl Listener {
    fn new(
        target: &EventTarget,
        name: &'static str,
        callback: impl FnMut(Event) + 'static,
    ) -> Result<Self, JsValue> {
        let callback = Closure::wrap(Box::new(callback) as Box<dyn FnMut(Event)>);
        target.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())?;
        Ok(Self {
            target: target.clone(),
            name,
            callback,
        })
    }
    fn detach(&self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.name, self.callback.as_ref().unchecked_ref());
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.detach();
    }
}

// No DOM events are dispatched. These registrations only invoke the supplied
// consumer callback after the authoritative consumer validates the entire frame.
struct EventHook {
    filter: EventFilter,
    callback: js_sys::Function,
}
struct ClickBinding {
    options: ClickOptions,
    callback: js_sys::Function,
    pairs: ClickPairs<HitStamp>,
}
enum Hook {
    Event(EventHook),
    Click(ClickBinding),
}
#[derive(Clone, PartialEq)]
struct HitStamp {
    target: Element,
    target_rect: [f64; 4],
    canvas: CanvasGeometry,
    revision: u64,
}
struct ClickDelivery {
    token: String,
    stamp: HitStamp,
    point: [f64; 2],
    payload: String,
}

struct Inner {
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    status: HtmlElement,
    users: HtmlSelectElement,
    pointer_toggle: HtmlInputElement,
    key_toggle: HtmlInputElement,
    endpoint: HtmlInputElement,
    credential: HtmlInputElement,
    connect_button: HtmlButtonElement,
    refresh_button: HtmlButtonElement,
    state: ClientState,
    local: LocalInput,
    ready: Option<EventReady>,
    socket: Option<WebSocket>,
    subscription: Option<SubscriptionId>,
    pending: BTreeSet<u64>,
    pending_hosted: BTreeMap<u64, (u64, Option<(HostRevision, HostedAction)>)>,
    hosted_receipt: mux_application_sdk::client::HostedReceipt,
    pending_windows: BTreeMap<u64, PendingWindow>,
    next_request: u64,
    authenticated: bool,
    awaiting_subscription: bool,
    // Only lives until onopen sends it. Never logged, returned or persisted.
    credential_once: Option<String>,
    listeners: Vec<Listener>,
    socket_listeners: Vec<Listener>,
    resize_observer: Option<ResizeObserver>,
    resize_callback: Option<Closure<dyn FnMut(js_sys::Array, ResizeObserver)>>,
    event_handler: Option<js_sys::Function>,
    notification: Option<String>,
    binding_root: Element,
    hooks: Registry<Hook>,
    click_deliveries: Vec<ClickDelivery>,
    dispatching: bool,
    generation: u64,
    disposed: bool,
    layout_revision: u64,
    demo_callbacks: Vec<Closure<dyn FnMut(String)>>,
}

#[derive(Clone, Copy)]
enum PendingWindow {
    Query {
        window: WindowId,
        remote: bool,
    },
    Control {
        window: WindowId,
        action: WindowAction,
        remote: bool,
    },
}

impl Inner {
    fn clear_clicks(&mut self) {
        self.layout_revision = self.layout_revision.saturating_add(1);
        self.click_deliveries.clear();
        for hook in self.hooks.values_mut() {
            if let Hook::Click(binding) = hook {
                binding.pairs.clear();
            }
        }
    }

    fn fence_hooks(&mut self) {
        // Saturation permanently fences new registration/connection, rather than
        // ever reusing a dispatch generation.
        self.generation = self.generation.saturating_add(1);
        self.clear_clicks();
    }

    fn click_geometry(&self) -> Result<CanvasGeometry, &'static str> {
        let window = web_sys::window().ok_or("Browser window unavailable")?;
        let mut element: Option<Element> = Some(self.canvas.clone().into());
        let mut depth = 0;
        while let Some(current) = element {
            depth += 1;
            if depth > 128 {
                return Err("Canvas ancestry exceeds the virtual hit-test bound");
            }
            let style = window
                .get_computed_style(&current)
                .map_err(|_| "Canvas computed geometry unavailable")?
                .ok_or("Canvas computed geometry unavailable")?;
            for property in [
                "transform",
                "perspective",
                "translate",
                "rotate",
                "scale",
                "offset-path",
            ] {
                let value = style
                    .get_property_value(property)
                    .map_err(|_| "Canvas computed geometry unavailable")?;
                if !value.is_empty() && value != "none" {
                    return Err("Virtual bindings require an untransformed canvas ancestry");
                }
            }
            let zoom = style
                .get_property_value("zoom")
                .map_err(|_| "Canvas computed geometry unavailable")?;
            if !matches!(zoom.as_str(), "" | "1" | "normal") {
                return Err(
                    "CSS zoom is unavailable for virtual bindings; browser DPR zoom is supported",
                );
            }
            if depth == 1 {
                for property in [
                    "padding-left",
                    "padding-right",
                    "padding-top",
                    "padding-bottom",
                    "border-left-width",
                    "border-right-width",
                    "border-top-width",
                    "border-bottom-width",
                ] {
                    let value = style
                        .get_property_value(property)
                        .map_err(|_| "Canvas computed geometry unavailable")?;
                    if value != "0px" {
                        return Err("Virtual bindings require zero canvas border and padding");
                    }
                }
            }
            element = current.parent_element();
        }
        self.geometry()
    }

    fn collect_clicks(&mut self, message: &ServerMessage) {
        let ServerMessage::EventStream {
            stream: Poll::Events { events },
            ..
        } = message
        else {
            return;
        };
        for envelope in events {
            match &envelope.event {
                ApplicationEvent::ScreensChanged { .. } => self.clear_clicks(),
                ApplicationEvent::UserDisposed { user }
                | ApplicationEvent::UserUpdated {
                    user: UserView { id: user, .. },
                } => {
                    // A replaced view may remove pointer consent or reset holds.
                    for hook in self.hooks.values_mut() {
                        if let Hook::Click(binding) = hook {
                            binding.pairs.retire(*user);
                        }
                    }
                    self.click_deliveries.clear();
                }
                ApplicationEvent::Input {
                    user,
                    input: Input::Button { button, transition },
                    position: Some(position),
                    transform_revision,
                    ..
                } => {
                    if !self.is_live()
                        || self.layout_revision == u64::MAX
                        || !self.state.users().contains_key(user)
                    {
                        continue;
                    }
                    let (Ok(geometry), Some(screens)) =
                        (self.click_geometry(), self.state.screens())
                    else {
                        self.clear_clicks();
                        continue;
                    };
                    if screens.revision != *transform_revision {
                        self.clear_clicks();
                        continue;
                    }
                    let Ok(local) = geometry.transform(screens).and_then(|transform| {
                        transform
                            .to_canvas(*position, *transform_revision)
                            .map_err(|_| "Invalid virtual pointer transform")
                    }) else {
                        self.clear_clicks();
                        continue;
                    };
                    let point = [local[0] + geometry.left, local[1] + geometry.top];
                    let root = self.binding_root.clone();
                    for token in self.hooks.ids() {
                        let Some(Hook::Click(binding)) = self.hooks.entries_mut(&token) else {
                            continue;
                        };
                        if binding.options.button != *button
                            || binding.options.user.is_some_and(|wanted| wanted != *user)
                        {
                            continue;
                        }
                        let stamp = hit_stamp(
                            &root,
                            &binding.options.selector,
                            geometry,
                            *transform_revision,
                            point,
                        );
                        let clicked = binding.pairs.transition(*user, *transition, stamp.clone());
                        if clicked && let Some(stamp) = stamp {
                            if self.click_deliveries.len() >= 256 {
                                self.clear_clicks();
                                return;
                            }
                            self.click_deliveries.push(ClickDelivery {
                                token,
                                stamp,
                                point,
                                payload: serde_json::json!({
                                    "kind": "virtual_click", "source": "authorized_virtual_stream",
                                    "selector": binding.options.selector, "event": envelope,
                                })
                                .to_string(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn notice(&mut self, text: &str) {
        self.status.set_text_content(Some(text));
    }

    fn send(&mut self, command: ClientCommand) -> Result<(), &'static str> {
        if !self.authenticated {
            return Err("Event connection is not authenticated");
        }
        let socket = self.socket.as_ref().ok_or("Event connection is closed")?;
        if socket.ready_state() != WebSocket::OPEN {
            return Err("Event connection is closed");
        }
        if self.pending.len() >= MAX_PENDING || socket.buffered_amount() > MAX_BUFFERED {
            return Err("Event connection is backpressured; reconnect explicitly");
        }
        let request_id = self
            .next_request
            .checked_add(1)
            .ok_or("Request limit reached")?;
        let hosted_pending = match &command {
            ClientCommand::HostedStateQuery { epoch } => Some((*epoch, None)),
            ClientCommand::HostedSetMode { reference, mode } => Some((
                reference.epoch,
                Some((*reference, HostedAction::SetMode(*mode))),
            )),
            ClientCommand::HostedViewport { reference, action } => Some((
                reference.epoch,
                Some((*reference, HostedAction::Viewport(*action))),
            )),
            _ => None,
        };
        if let Some((epoch, action)) = &hosted_pending {
            let ready = self.ready.as_ref().ok_or("Missing host approval")?;
            if !self.is_live() || !ready.events.host_state || *epoch != ready.epoch {
                return Err("Hosted observation is unavailable or stale");
            }
            if let Some((reference, action)) = action {
                let granted = match action {
                    HostedAction::SetMode(_) => ready.controls.host_mode_control,
                    HostedAction::Viewport(_) => ready.controls.host_viewport_control,
                };
                if !granted
                    || !self.hosted_receipt.permits(&self.state, *reference)
                    || self
                        .pending_hosted
                        .values()
                        .any(|(_, action)| action.is_some())
                {
                    return Err("Hosted action requires current observation and distinct approval");
                }
            }
        }
        let window_pending = match &command {
            ClientCommand::WindowQuery { window } | ClientCommand::WindowTrack { window } => {
                Some(PendingWindow::Query {
                    window: *window,
                    remote: false,
                })
            }
            ClientCommand::RemoteWindowQuery { reference }
            | ClientCommand::RemoteWindowTrack { reference } => Some(PendingWindow::Query {
                window: reference.window,
                remote: true,
            }),
            ClientCommand::WindowControl { window, control } => Some(PendingWindow::Control {
                window: *window,
                action: control.action(),
                remote: false,
            }),
            ClientCommand::RemoteWindowControl { reference, control } => {
                Some(PendingWindow::Control {
                    window: reference.window,
                    action: control.action(),
                    remote: true,
                })
            }
            _ => None,
        };
        let message = ClientMessage {
            request_id,
            request: ClientRequest {
                version: PROTOCOL_VERSION,
                command,
            },
        };
        let json = serde_json::to_string(&message).map_err(|_| "Could not encode typed request")?;
        let limit = self
            .ready
            .as_ref()
            .map_or(MAX_REQUEST_BYTES, |ready| ready.limits.max_request_bytes);
        if json.len() > limit.min(MAX_REQUEST_BYTES) {
            return Err("Request exceeds event protocol limit");
        }
        if socket.send_with_str(&json).is_err() {
            if hosted_pending
                .as_ref()
                .is_some_and(|(_, action)| action.is_some())
                || matches!(window_pending, Some(PendingWindow::Control { .. }))
            {
                self.stop(
                    "Window action outcome unconfirmed after send failure; reconnect and inspect",
                    false,
                );
                return Err("Window action outcome unconfirmed; connection fenced");
            }
            return Err("Event socket send failed");
        }
        if let Some(expected) = hosted_pending {
            self.pending_hosted.insert(request_id, expected);
        }
        self.next_request = request_id;
        self.pending.insert(request_id);
        if let Some(window_pending) = window_pending {
            self.pending_windows.insert(request_id, window_pending);
        }
        Ok(())
    }

    fn is_live(&self) -> bool {
        self.authenticated && self.state.state() == &StreamState::Live
    }

    fn current_user(&self, keys: bool) -> Result<UserId, &'static str> {
        if !self.is_live() {
            return Err("A fresh subscribed snapshot is required");
        }
        let user = self
            .local
            .selected
            .ok_or("Select a user explicitly before enabling input")?;
        let view = self
            .state
            .users()
            .get(&user)
            .ok_or("Selected user is no longer visible")?;
        let ready = self.ready.as_ref().ok_or("Event authority is missing")?;
        if !selects(&ready.controls, view) {
            return Err("Selected user is outside the control grant");
        }
        if keys {
            if !self.local.keys_enabled || !ready.controls.key_input || view.held_keys.is_none() {
                return Err("Keyboard input needs its separate grant and enable control");
            }
        } else if !self.local.pointer_enabled
            || !ready.controls.pointer_input
            || view.pointer.is_none()
        {
            return Err("Pointer input needs its grant and enable control");
        }
        Ok(user)
    }

    fn input(&mut self, user: UserId, input: Input) -> Result<(), &'static str> {
        let transform_revision = self
            .state
            .screens()
            .ok_or("Screen transform unavailable")?
            .revision;
        self.send(ClientCommand::InputAtRevision {
            user,
            input,
            transform_revision,
        })?;
        self.local.record(user, input);
        Ok(())
    }

    fn release(&mut self, authorized: bool) {
        let grant = if authorized && self.is_live() {
            self.ready.as_ref().map(|ready| &ready.controls)
        } else {
            None
        };
        let releases = self.local.drain_releases(grant, self.state.users());
        for command in releases {
            if self.send(command).is_err() {
                // A release that cannot be queued must retire this controller's
                // server-side holds. Never leave the live connection holding it.
                self.stop("Could not release held input; connection retired", false);
                break;
            }
        }
    }

    fn disable_inputs(&mut self) {
        self.local.disable();
        self.pointer_toggle.set_checked(false);
        self.key_toggle.set_checked(false);
        self.pointer_toggle.set_disabled(true);
        self.key_toggle.set_disabled(true);
        self.users.set_value("");
    }

    fn stop(&mut self, message: &str, graceful: bool) {
        let message = if !self
            .pending_windows
            .values()
            .any(|pending| matches!(pending, PendingWindow::Control { .. }))
        {
            message.to_owned()
        } else {
            format!("Window action outcome unconfirmed; inspect before retrying. {message}")
        };
        self.fence_hooks();
        self.release(graceful);
        if graceful && self.authenticated {
            if let Some(subscription) = self.subscription.take() {
                let _ = self.send(ClientCommand::Unsubscribe { subscription });
            }
            let _ = self.send(ClientCommand::Disconnect);
        }
        self.authenticated = false;
        self.awaiting_subscription = false;
        self.credential_once = None;
        self.credential.set_value("");
        self.ready = None;
        self.pending.clear();
        self.pending_hosted.clear();
        self.hosted_receipt.clear();
        self.pending_windows.clear();
        self.subscription = None;
        self.state.disconnect();
        self.disable_inputs();
        // Detach before close so a deliberate disconnect is not reported twice.
        // Retain closure allocations until the next explicit connect, safely
        // outside any active WebSocket callback.
        for listener in &self.socket_listeners {
            listener.detach();
        }
        if let Some(socket) = self.socket.take() {
            let _ = socket.close();
        }
        self.connect_button.set_disabled(false);
        self.refresh_button.set_disabled(true);
        self.users.set_inner_html("");
        self.notice(&message);
        self.notification =
            Some(serde_json::json!({"kind": "local_disconnected", "message": message}).to_string());
        let _ = self.draw();
    }

    fn geometry(&self) -> Result<CanvasGeometry, &'static str> {
        let bounds = self.canvas.get_bounding_client_rect();
        let geometry = CanvasGeometry {
            left: bounds.left(),
            top: bounds.top(),
            width: bounds.width(),
            height: bounds.height(),
            pixel_ratio: web_sys::window()
                .ok_or("Browser window unavailable")?
                .device_pixel_ratio(),
        };
        geometry.backing_size()?;
        Ok(geometry)
    }

    fn motion(&mut self, event: &PointerEvent) -> Result<(), &'static str> {
        if event.pointer_type() != "mouse" {
            return Err("Pen and touch are unavailable; use a virtual mouse");
        }
        let user = self.current_user(false)?;
        let screens = self.state.screens().ok_or("Screen transform unavailable")?;
        let point = self.geometry()?.to_screen(
            [f64::from(event.client_x()), f64::from(event.client_y())],
            screens,
            screens.revision,
        )?;
        if self
            .state
            .users()
            .get(&user)
            .and_then(|view| view.cursor.as_ref())
            .is_some_and(|cursor| cursor.movement_locked)
        {
            return Err("Selected user's pointer movement is locked");
        }
        self.input(user, Input::Motion { position: point })
    }

    fn update_controls(&mut self) {
        let live = self.is_live();
        let selected = self
            .local
            .selected
            .and_then(|id| self.state.users().get(&id));
        let pointer = selected.is_some_and(|user| {
            user.pointer.is_some()
                && self.ready.as_ref().is_some_and(|ready| {
                    ready.controls.pointer_input && selects(&ready.controls, user)
                })
        });
        let keys = selected.is_some_and(|user| {
            user.held_keys.is_some()
                && self
                    .ready
                    .as_ref()
                    .is_some_and(|ready| ready.controls.key_input && selects(&ready.controls, user))
        });
        self.pointer_toggle.set_disabled(!live || !pointer);
        self.key_toggle.set_disabled(!live || !keys);
        self.refresh_button
            .set_disabled(!self.authenticated || self.awaiting_subscription);
        if !pointer {
            self.local.pointer_enabled = false;
            self.pointer_toggle.set_checked(false);
        }
        if !keys {
            self.local.keys_enabled = false;
            self.key_toggle.set_checked(false);
        }
    }

    fn refresh(&mut self) -> Result<(), &'static str> {
        let capacity = self
            .ready
            .as_ref()
            .map_or(256, |ready| ready.limits.max_queue.min(256));
        self.refresh_with_capacity(capacity)
    }

    fn refresh_with_capacity(&mut self, capacity: usize) -> Result<(), &'static str> {
        // Validate app-selected backpressure before releasing input, fencing
        // callbacks, clearing state, or admitting any network request.
        let ready = self.ready.as_ref().ok_or("Missing event authority")?;
        if capacity == 0 || capacity > ready.limits.max_queue || capacity > MAX_QUEUE {
            return Err("Subscription capacity exceeds the current event authority bound");
        }
        if self.awaiting_subscription {
            return Err("A fresh subscription snapshot is already pending");
        }
        self.fence_hooks();
        self.release(true);
        self.disable_inputs();
        self.state.disconnect();
        self.hosted_receipt.clear();
        let command = match self.subscription {
            Some(subscription) => ClientCommand::Resubscribe {
                subscription,
                capacity,
            },
            None => ClientCommand::Subscribe { capacity },
        };
        self.send(command)?;
        self.awaiting_subscription = true;
        self.refresh_button.set_disabled(true);
        self.notice("Waiting for a fresh subscription snapshot");
        self.rebuild_users()?;
        self.draw()
    }

    fn rebuild_users(&mut self) -> Result<(), &'static str> {
        let document = self.canvas.owner_document().ok_or("Document unavailable")?;
        self.users.set_inner_html("");
        let placeholder = document
            .create_element("option")
            .map_err(|_| "Could not create user selector")?;
        placeholder
            .set_attribute("value", "")
            .map_err(|_| "Could not create user selector")?;
        placeholder.set_text_content(Some("Choose a user to control"));
        self.users
            .append_child(&placeholder)
            .map_err(|_| "Could not create user selector")?;
        for user in self.state.users().values() {
            let option = document
                .create_element("option")
                .map_err(|_| "Could not create user selector")?;
            option
                .set_attribute("value", &format!("{}:{}", user.id.epoch, user.id.local))
                .map_err(|_| "Could not create user selector")?;
            option.set_text_content(Some(&format!(
                "{} · user {}:{}",
                user.name, user.id.epoch, user.id.local
            )));
            self.users
                .append_child(&option)
                .map_err(|_| "Could not create user selector")?;
        }
        if let Some(user) = self.local.selected {
            if self.state.users().contains_key(&user) {
                self.users
                    .set_value(&format!("{}:{}", user.epoch, user.local));
            } else {
                self.release(false);
                self.disable_inputs();
            }
        }
        self.update_controls();
        Ok(())
    }

    fn receive(&mut self, data: &str) -> Result<(), &'static str> {
        if data.len() > MAX_RESPONSE_BYTES {
            return Err("Event frame exceeds the protocol bound");
        }
        let message: ServerMessage =
            serde_json::from_str(data).map_err(|_| "Invalid typed event frame")?;
        let accepted = message.clone();
        match message {
            ServerMessage::Ready { ready } => {
                if self.authenticated
                    || ready.version != PROTOCOL_VERSION
                    || ready.epoch == 0
                    || ready.limits.max_request_bytes == 0
                    || ready.limits.max_poll_events == 0
                    || ready.limits.max_poll_events > MAX_POLL_EVENTS
                    || ready.limits.max_queue == 0
                    || ready.limits.max_queue > MAX_QUEUE
                {
                    return Err("Invalid or duplicate event handshake");
                }
                self.authenticated = true;
                self.ready = Some(ready);
                self.notice("Authenticated; waiting for a fresh snapshot");
                let capacity = self
                    .ready
                    .as_ref()
                    .ok_or("Missing handshake")?
                    .limits
                    .max_queue
                    .min(256);
                self.send(ClientCommand::Subscribe { capacity })?;
                self.awaiting_subscription = true;
            }
            ServerMessage::Response { request_id, reply } => {
                if !self.authenticated || !self.pending.remove(&request_id) {
                    return Err("Unexpected response identifier");
                }
                let expected_hosted = self.pending_hosted.remove(&request_id);
                let expected_window = self.pending_windows.remove(&request_id);
                match reply {
                    Reply::Ok { version, response } => {
                        if version != PROTOCOL_VERSION {
                            return Err("Response version mismatch");
                        }
                        if matches!(expected_window, Some(PendingWindow::Control { .. }))
                            && !matches!(&response, ClientResponse::WindowControlled { .. })
                        {
                            return Err("Window action receipt was not observed");
                        }
                        if matches!(expected_window, Some(PendingWindow::Query { .. }))
                            && !matches!(&response, ClientResponse::Window { .. })
                        {
                            return Err("Window observation was not received");
                        }
                        match (&expected_hosted, &response) {
                            (Some((epoch, None)), ClientResponse::HostedState { state })
                                if self
                                    .ready
                                    .as_ref()
                                    .is_some_and(|ready| ready.epoch == *epoch)
                                    && state.revision != 0 => {}
                            (
                                Some((epoch, Some((reference, expected)))),
                                ClientResponse::HostedAction { action, state },
                            ) if self
                                .ready
                                .as_ref()
                                .is_some_and(|ready| ready.epoch == *epoch)
                                && action == expected
                                && state.revision >= reference.revision
                                && match action {
                                    HostedAction::SetMode(mode) => {
                                        state.mode == Some(*mode)
                                            && state.phase
                                                == if *mode == HostedMode::Native {
                                                    HostedPhase::Native
                                                } else {
                                                    HostedPhase::Ready
                                                }
                                    }
                                    HostedAction::Viewport(action) => {
                                        state.minimized
                                            == Some(matches!(action, HostViewportAction::Minimize))
                                    }
                                } => {}
                            (
                                None,
                                ClientResponse::HostedState { .. }
                                | ClientResponse::HostedAction { .. },
                            )
                            | (Some(_), _) => {
                                return Err(
                                    "Hosted receipt does not match pending request; outcome unconfirmed",
                                );
                            }
                            _ => {}
                        }
                        match response {
                            ClientResponse::Subscribed {
                                subscription,
                                snapshot,
                            } => {
                                if !self.awaiting_subscription {
                                    return Err("Unexpected subscription snapshot");
                                }
                                let epoch = self.ready.as_ref().ok_or("Missing handshake")?.epoch;
                                self.state
                                    .install_snapshot(snapshot, epoch)
                                    .map_err(|_| "Fresh snapshot validation failed")?;
                                self.hosted_receipt.clear();
                                self.clear_clicks();
                                self.subscription = Some(subscription);
                                self.awaiting_subscription = false;
                                self.notice("Live virtual events. Select a user and explicitly enable controls");
                                self.rebuild_users()?;
                            }
                            ClientResponse::Snapshot { snapshot } => {
                                // Queries do not establish the stream, but their
                                // payload still must validate before notification.
                                let epoch = self.ready.as_ref().ok_or("Missing handshake")?.epoch;
                                validate_snapshot_query(snapshot, epoch)
                                    .map_err(|_| "Snapshot query validation failed")?;
                                self.notice("Snapshot query acknowledged; subscribe/refresh establishes the live stream");
                            }
                            ClientResponse::HostedState { state }
                            | ClientResponse::HostedAction { state, .. } => {
                                if self.is_live() {
                                    let epoch = expected_hosted
                                        .as_ref()
                                        .map(|(epoch, _)| *epoch)
                                        .ok_or("Hosted receipt lacked request ownership")?;
                                    self.hosted_receipt
                                        .observe(epoch, &state, &self.state)
                                        .map_err(
                                            |_| "Hosted receipt conflicts with current stream",
                                        )?;
                                }
                                self.notice("Hosted receipt validated; observe current state before another action");
                            }
                            ClientResponse::Polled { .. } => {
                                return Err("Unexpected poll response on server-pushed stream");
                            }
                            ClientResponse::Window { window } => {
                                let Some(PendingWindow::Query {
                                    window: expected,
                                    remote,
                                }) = expected_window
                                else {
                                    return Err("Window observation had no matching request");
                                };
                                if !window.bounds.valid_region()
                                    || window.metadata.validate().is_err()
                                    || window.id != expected
                                    || remote
                                        != matches!(
                                            window.observation.source,
                                            WindowObservationSource::NativeClientReported
                                        )
                                {
                                    return Err("Invalid window observation");
                                }
                                self.notice(if window.observation.stale {
                                    "Window observation is stale; refresh with its native owner"
                                } else if remote {
                                    "Native-window observation received"
                                } else {
                                    "Owned-window observation received"
                                });
                            }
                            ClientResponse::WindowControlled { acknowledgement } => {
                                let Some(PendingWindow::Control {
                                    window: expected,
                                    action,
                                    remote,
                                }) = expected_window
                                else {
                                    return Err("Window action had no matching request");
                                };
                                if acknowledgement.window != acknowledgement.observed.id
                                    || acknowledgement.window != expected
                                    || acknowledgement.action != action
                                    || !acknowledgement.observed.bounds.valid_region()
                                    || acknowledgement.observed.metadata.validate().is_err()
                                    || (remote && acknowledgement.observed.observation.stale)
                                    || remote
                                        != matches!(
                                            acknowledgement.observed.observation.source,
                                            WindowObservationSource::NativeClientReported
                                        )
                                {
                                    return Err("Invalid window action receipt");
                                }
                                self.notice(if remote {
                                    "Native-window action observed by its owner"
                                } else {
                                    "Owned-window action observed"
                                });
                            }
                            ClientResponse::Disconnected => {
                                self.stop("Server disconnected; reconnect explicitly", false);
                            }
                            _ => { /* Acknowledgements do not mutate authoritative rendering. */ }
                        }
                    }
                    Reply::Error { version, error } => {
                        if expected_hosted
                            .as_ref()
                            .is_some_and(|(_, action)| action.is_some())
                            && error.code == FailureCode::Uncertain
                        {
                            return Err(
                                "Hosted action outcome uncertain; reconnect and inspect without replay",
                            );
                        }
                        if version != PROTOCOL_VERSION {
                            return Err("Response version mismatch");
                        }
                        if error.code == FailureCode::Uncertain {
                            self.fence_hooks();
                            self.release(true);
                            self.disable_inputs();
                            self.state.disconnect();
                            self.hosted_receipt.clear();
                            self.notice("Window action outcome unconfirmed. Refresh the subscription after owner observation; do not retry automatically");
                            self.update_controls();
                        } else {
                            // Any rejected input can invalidate locally predicted holds.
                            // Close and let the server retire this controller's holds.
                            let notice = format!(
                                "Request rejected ({:?}); reconnect explicitly",
                                error.code
                            );
                            self.stop(&notice, false);
                        }
                    }
                }
            }
            ServerMessage::EventStream {
                subscription,
                stream,
            } => {
                // A resubscribe request fences old queued frames. The new atomic
                // snapshot incorporates their final state; never replay them.
                if self.awaiting_subscription && self.subscription == Some(subscription) {
                    return Ok(());
                }
                if !self.authenticated || self.subscription != Some(subscription) {
                    return Err("Stream subscription mismatch");
                }
                if matches!(stream, Poll::Gap { .. }) {
                    self.fence_hooks();
                    self.release(true);
                    self.hosted_receipt.clear();
                }
                match self
                    .state
                    .fold_poll(stream)
                    .map_err(|_| "Invalid ordered event stream; reconnect explicitly")?
                {
                    FoldOutcome::Applied(_) => {
                        self.hosted_receipt
                            .reconcile(&self.state)
                            .map_err(|_| "Hosted event conflicts with prior receipt")?;
                        self.rebuild_users()?;
                    }
                    FoldOutcome::NeedsSnapshot => {
                        self.hosted_receipt.clear();
                        self.release(false);
                        self.disable_inputs();
                        self.notice("Sequence gap: state cleared. Use Refresh subscription for a fresh snapshot");
                    }
                    FoldOutcome::Ended(_) => {
                        self.hosted_receipt.clear();
                        self.stop(
                            "Event authority ended; reconnect explicitly after host approval",
                            false,
                        );
                    }
                }
            }
            ServerMessage::Disconnected { .. } => {
                self.stop(
                    "Event authority ended; reconnect explicitly after host approval",
                    false,
                );
            }
            ServerMessage::WindowApply { .. } | ServerMessage::OwnerResponse { .. } => {
                return Err("Native-owner frame sent to browser receiver");
            }
        }
        self.collect_clicks(&accepted);
        self.draw()?;
        // The public handler receives only successfully validated frames. It is
        // invoked after dropping RefCell borrows so it may call SDK methods.
        self.notification = Some(data.into());
        Ok(())
    }

    fn draw(&self) -> Result<(), &'static str> {
        let geometry = self.geometry()?;
        let [width, height] = geometry.backing_size()?;
        if self.canvas.width() != width {
            self.canvas.set_width(width);
        }
        if self.canvas.height() != height {
            self.canvas.set_height(height);
        }
        let scale_x = f64::from(width) / geometry.width;
        let scale_y = f64::from(height) / geometry.height;
        self.context
            .set_transform(scale_x, 0.0, 0.0, scale_y, 0.0, 0.0)
            .map_err(|_| "Canvas transform failed")?;
        self.context.set_fill_style_str("#111b2b");
        self.context
            .fill_rect(0.0, 0.0, geometry.width, geometry.height);
        self.context.set_font("14px sans-serif");
        self.context.set_fill_style_str("#cbd8eb");
        let _ = self.context.fill_text(
            &format!(
                "Virtual cooperative canvas · {} users · sequence {}",
                self.state.users().len(),
                self.state.last_sequence()
            ),
            16.0,
            26.0,
        );
        let Some(screens) = self.state.screens() else {
            return Ok(());
        };
        let transform = geometry.transform(screens)?;
        for user in self.state.users().values() {
            let (Some(pointer), Some(cursor)) = (&user.pointer, &user.cursor) else {
                continue;
            };
            if !cursor.visible {
                continue;
            }
            let [x, y] = transform
                .to_canvas(pointer.position, screens.revision)
                .map_err(|_| "Stale rendering transform")?;
            let [red, green, blue, alpha] = cursor.color;
            let color = format!("rgba({red},{green},{blue},{})", f64::from(alpha) / 255.0);
            let size = f64::from(cursor.size);
            self.context.set_fill_style_str(&color);
            self.context.set_stroke_style_str(&color);
            self.context.set_line_width(2.0);
            self.context.begin_path();
            match cursor.shape {
                CursorShape::Arrow => {
                    self.context.move_to(x, y);
                    self.context.line_to(x + size * 0.25, y + size);
                    self.context.line_to(x + size * 0.45, y + size * 0.60);
                    self.context.line_to(x + size * 0.85, y + size * 0.55);
                    self.context.close_path();
                    self.context.fill();
                }
                CursorShape::Crosshair => {
                    self.context.move_to(x - size / 2.0, y);
                    self.context.line_to(x + size / 2.0, y);
                    self.context.move_to(x, y - size / 2.0);
                    self.context.line_to(x, y + size / 2.0);
                    self.context.stroke();
                }
                CursorShape::IBeam => {
                    self.context.move_to(x, y - size / 2.0);
                    self.context.line_to(x, y + size / 2.0);
                    self.context.move_to(x - size / 4.0, y - size / 2.0);
                    self.context.line_to(x + size / 4.0, y - size / 2.0);
                    self.context.move_to(x - size / 4.0, y + size / 2.0);
                    self.context.line_to(x + size / 4.0, y + size / 2.0);
                    self.context.stroke();
                }
                CursorShape::Ring => {
                    self.context
                        .arc(x, y, size / 2.0, 0.0, std::f64::consts::TAU)
                        .map_err(|_| "Cursor render failed")?;
                    self.context.stroke();
                }
                CursorShape::System => { /* Validated shared state rejects this unavailable renderer. */
                }
            }
            let _ = self.context.fill_text(
                &format!(
                    "{} [{}:{}] · buttons {} · keys {}",
                    user.name,
                    user.id.epoch,
                    user.id.local,
                    pointer.held_buttons.len(),
                    user.held_keys.as_ref().map_or(0, Vec::len)
                ),
                x + size + 4.0,
                y + 14.0,
            );
        }
        Ok(())
    }
}

/// Owns all browser registrations. `free()`/`dispose()` closes the subscription,
/// removes every DOM listener and disconnects ResizeObserver. No global key input.
#[wasm_bindgen]
pub struct WebClient {
    inner: Rc<RefCell<Inner>>,
}

#[wasm_bindgen]
impl WebClient {
    /// Explicit connection API. Credentials pass directly into transient Rust
    /// memory, without requiring the demonstration password element.
    pub fn connect(&self, endpoint: &str, event_credential: &str) -> Result<(), JsValue> {
        open_connection(&self.inner, endpoint.into(), event_credential.into())
            .map_err(JsValue::from_str)
    }

    pub fn disconnect(&self) {
        self.inner.borrow_mut().stop(
            "Disconnected; reconnect explicitly with an event credential",
            true,
        );
        notify(&self.inner);
    }

    /// Receives validated typed ServerMessage JSON plus local_disconnected.
    /// Passing undefined unregisters it. No credentials occur in these frames.
    pub fn set_event_handler(&self, handler: Option<js_sys::Function>) {
        let mut inner = self.inner.borrow_mut();
        if !inner.disposed {
            inner.event_handler = handler;
        }
    }

    /// Register a consumer callback with a JSON EventFilter. The callback receives
    /// the original validated JSON frame, once per matching frame (not per event).
    /// It grants no input authority. Returned opaque text is never a JS Number.
    pub fn add_event_listener(
        &self,
        filter_json: &str,
        callback: js_sys::Function,
    ) -> Result<String, JsValue> {
        if filter_json.len() > 512 {
            return Err(JsValue::from_str("Event filter exceeds its bound"));
        }
        let filter: EventFilter = serde_json::from_str(filter_json)
            .map_err(|_| JsValue::from_str("Invalid event filter"))?;
        let mut inner = self.inner.borrow_mut();
        if inner.disposed || inner.generation == u64::MAX {
            return Err(JsValue::from_str(
                "Client is disposed or dispatch identities exhausted",
            ));
        }
        inner
            .hooks
            .insert(Hook::Event(EventHook { filter, callback }))
            .map_err(JsValue::from_str)
    }

    /// Register explicit virtual activation observation for one selector's first
    /// match inside this client's mounted root. No HTMLElement.click is called.
    pub fn bind_virtual_click(
        &self,
        options_json: &str,
        callback: js_sys::Function,
    ) -> Result<String, JsValue> {
        if options_json.len() > 1024 {
            return Err(JsValue::from_str("Virtual binding exceeds its bound"));
        }
        let options: ClickOptions = serde_json::from_str(options_json)
            .map_err(|_| JsValue::from_str("Invalid virtual click options"))?;
        if options.selector.is_empty() || options.selector.len() > 512 {
            return Err(JsValue::from_str("Selector must contain 1 to 512 bytes"));
        }
        let mut inner = self.inner.borrow_mut();
        if inner.disposed || inner.generation == u64::MAX {
            return Err(JsValue::from_str(
                "Client is disposed or dispatch identities exhausted",
            ));
        }
        inner.binding_root.query_selector(&options.selector)?;
        inner.click_geometry().map_err(JsValue::from_str)?;
        inner
            .hooks
            .insert(Hook::Click(ClickBinding {
                options,
                callback,
                pairs: ClickPairs::default(),
            }))
            .map_err(JsValue::from_str)
    }

    /// Clear all pending virtual presses after application-owned layout changes.
    /// Use this for target mutations that occur and revert between stream frames.
    pub fn invalidate_virtual_clicks(&self) {
        self.inner.borrow_mut().clear_clicks();
    }

    /// Idempotent removal for either registration type. Removes pending callbacks.
    pub fn remove_listener(&self, token: &str) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner
            .click_deliveries
            .retain(|delivery| delivery.token != token);
        inner.hooks.remove(token)
    }

    pub fn refresh_subscription(&self) -> Result<(), JsValue> {
        self.inner.borrow_mut().refresh().map_err(JsValue::from_str)
    }

    /// Explicitly select this subscription's bounded event queue capacity.
    /// Valid values are 1 through the authenticated Ready max_queue and Core's
    /// MAX_QUEUE. Invalid capacities leave input, callbacks and requests intact.
    /// Success releases held input and requires a fresh snapshot and explicit
    /// user/input selection, like refresh_subscription(). No automatic recovery.
    pub fn refresh_subscription_with_capacity(&self, capacity: u32) -> Result<(), JsValue> {
        self.inner
            .borrow_mut()
            .refresh_with_capacity(capacity as usize)
            .map_err(JsValue::from_str)
    }

    /// Explicit user selection. Selection always disables both production gates.
    pub fn select_user(&self, user_json: &str) -> Result<(), JsValue> {
        if user_json.len() > 128 {
            return Err(JsValue::from_str("Invalid user identity"));
        }
        let user: UserId = serde_json::from_str(user_json)
            .map_err(|_| JsValue::from_str("Invalid user identity"))?;
        let mut inner = self.inner.borrow_mut();
        if !inner.is_live() || !inner.state.users().contains_key(&user) {
            return Err(JsValue::from_str(
                "User is not in the current authorized snapshot",
            ));
        }
        inner.release(true);
        inner.disable_inputs();
        inner.local.selected = Some(user);
        inner
            .users
            .set_value(&format!("{}:{}", user.epoch, user.local));
        inner.update_controls();
        Ok(())
    }

    pub fn enable_pointer(&self, enabled: bool) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        inner.release(true);
        inner.update_controls();
        if enabled && inner.pointer_toggle.disabled() {
            return Err(JsValue::from_str(
                "Select a user with pointer observation and control approval",
            ));
        }
        inner.pointer_toggle.set_checked(enabled);
        inner.local.pointer_enabled = enabled;
        Ok(())
    }

    pub fn enable_keyboard(&self, enabled: bool) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        inner.release(true);
        inner.update_controls();
        if enabled && inner.key_toggle.disabled() {
            return Err(JsValue::from_str(
                "Select a user with distinct keyboard observation and control approval",
            ));
        }
        inner.key_toggle.set_checked(enabled);
        inner.local.keys_enabled = enabled;
        Ok(())
    }

    /// An explicit API call for typed SDK controls. This only accepts ClientCommand;
    /// host registration, consent, window handles and grants cannot be expressed.
    pub fn request(&self, command_json: &str) -> Result<(), JsValue> {
        if command_json.len() > MAX_REQUEST_BYTES {
            return Err(JsValue::from_str("Command exceeds protocol bound"));
        }
        let command: ClientCommand = serde_json::from_str(command_json)
            .map_err(|_| JsValue::from_str("Invalid ClientCommand"))?;
        if matches!(
            command,
            ClientCommand::Subscribe { .. }
                | ClientCommand::Resubscribe { .. }
                | ClientCommand::Unsubscribe { .. }
                | ClientCommand::Poll { .. }
                | ClientCommand::Disconnect
        ) {
            return Err(JsValue::from_str(
                "Use explicit connection/refresh/disconnect lifecycle controls",
            ));
        }
        let mut inner = self.inner.borrow_mut();
        match &command {
            ClientCommand::RemoteWindowQuery { reference } => {
                if !inner.is_live()
                    || !inner
                        .ready
                        .as_ref()
                        .is_some_and(|ready| ready.events.owned_windows)
                    || inner
                        .ready
                        .as_ref()
                        .is_none_or(|ready| ready.epoch != reference.epoch)
                {
                    return Err(JsValue::from_str("Window observation is unavailable"));
                }
            }
            ClientCommand::RemoteWindowTrack { reference }
            | ClientCommand::RemoteWindowUntrack { reference } => {
                if !inner.is_live()
                    || !inner.ready.as_ref().is_some_and(|ready| {
                        ready.events.owned_windows && ready.controls.window_tracking
                    })
                    || inner
                        .ready
                        .as_ref()
                        .is_none_or(|ready| ready.epoch != reference.epoch)
                {
                    return Err(JsValue::from_str("Window tracking is unavailable"));
                }
            }
            ClientCommand::RemoteWindowControl { reference, .. } => {
                if !inner.is_live()
                    || !inner.ready.as_ref().is_some_and(|ready| {
                        ready.events.owned_windows
                            && ready.controls.window_tracking
                            && ready.controls.window_control
                            && ready.epoch == reference.epoch
                    })
                    || inner
                        .state
                        .windows()
                        .get(&reference.window)
                        .is_none_or(|info| info.observation.stale)
                {
                    return Err(JsValue::from_str(
                        "Track and refresh this approved window first",
                    ));
                }
            }
            _ => {}
        }
        let input = match &command {
            ClientCommand::Input { user, input }
            | ClientCommand::InputAtRevision { user, input, .. } => Some((*user, *input)),
            _ => None,
        };
        // Atomic click uses the explicit local pointer selection/enable gate as
        // well as the server-enforced per-user control grant. It has no local
        // held transition to record after its atomic down/up.
        if let ClientCommand::Click { user, .. } = &command
            && inner.current_user(false).map_err(JsValue::from_str)? != *user
        {
            return Err(JsValue::from_str(
                "Click user is not the selected pointer user",
            ));
        }
        let production = match (&command, input) {
            (_, Some((user, input))) => Some((user, input.is_key())),
            (ClientCommand::Click { user, .. }, None) => Some((*user, false)),
            _ => None,
        };
        if let Some((user, keys)) = production {
            let ready = inner
                .ready
                .as_ref()
                .ok_or_else(|| JsValue::from_str("Missing event authority"))?;
            if !inner.is_live()
                || !inner
                    .state
                    .users()
                    .get(&user)
                    .is_some_and(|view| selects(&ready.controls, view))
                || if keys {
                    !ready.controls.key_input
                } else {
                    !ready.controls.pointer_input
                }
            {
                return Err(JsValue::from_str("Input is outside the current grant"));
            }
            inner.send(command).map_err(JsValue::from_str)?;
            if let Some((user, input)) = input {
                inner.local.record(user, input);
            }
            Ok(())
        } else {
            let hosted = matches!(
                &command,
                ClientCommand::HostedStateQuery { .. }
                    | ClientCommand::HostedSetMode { .. }
                    | ClientCommand::HostedViewport { .. }
            );
            let mutation = matches!(
                &command,
                ClientCommand::HostedSetMode { .. } | ClientCommand::HostedViewport { .. }
            );
            inner.send(command).map_err(JsValue::from_str)?;
            if hosted {
                let id = inner.next_request;
                // Request IDs never repeat across refresh, gaps, or reconnects.
                // Dispatch generation only fences callbacks; it is not a request lifetime.
                let weak = Rc::downgrade(&self.inner);
                let callback = Closure::once_into_js(move || {
                    if let Some(inner) = weak.upgrade() {
                        let mut state = inner.borrow_mut();
                        if hosted_deadline_pending(&state.pending_hosted, id) {
                            state.stop("Hosted request deadline reached; action outcome unconfirmed; reconnect and inspect without replay", false);
                        }
                    }
                });
                let armed = web_sys::window().is_some_and(|window| {
                    window
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            callback.unchecked_ref(),
                            if mutation { 60_000 } else { 6_000 },
                        )
                        .is_ok()
                });
                if !armed {
                    inner.stop("Hosted deadline unavailable; action outcome unconfirmed; connection fenced", false);
                    return Err(JsValue::from_str("Hosted deadline unavailable"));
                }
            }
            Ok(())
        }
    }

    /// Query one GUI-approved native window. Its numeric ID comes from the GUI
    /// approval, never from a native handle. The validated response is delivered
    /// to the event handler with its request ID.
    pub fn query_window(&self, reference_json: &str) -> Result<(), JsValue> {
        let reference = serde_json::from_str::<RemoteWindowRef>(reference_json)
            .map_err(|_| JsValue::from_str("Invalid window reference"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::RemoteWindowQuery { reference })
                .map_err(|_| JsValue::from_str("Could not encode window query"))?,
        )
    }

    pub fn track_window(&self, reference_json: &str) -> Result<(), JsValue> {
        let reference = serde_json::from_str::<RemoteWindowRef>(reference_json)
            .map_err(|_| JsValue::from_str("Invalid window reference"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::RemoteWindowTrack { reference })
                .map_err(|_| JsValue::from_str("Could not encode window tracking"))?,
        )
    }

    pub fn untrack_window(&self, reference_json: &str) -> Result<(), JsValue> {
        let reference = serde_json::from_str::<RemoteWindowRef>(reference_json)
            .map_err(|_| JsValue::from_str("Invalid window reference"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::RemoteWindowUntrack { reference })
                .map_err(|_| JsValue::from_str("Could not encode window tracking"))?,
        )
    }

    pub fn control_window(&self, reference_json: &str, control_json: &str) -> Result<(), JsValue> {
        let reference = serde_json::from_str::<RemoteWindowRef>(reference_json)
            .map_err(|_| JsValue::from_str("Invalid window reference"))?;
        let control = serde_json::from_str::<WindowControl>(control_json)
            .map_err(|_| JsValue::from_str("Invalid window action"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::RemoteWindowControl { reference, control })
                .map_err(|_| JsValue::from_str("Could not encode window action"))?,
        )
    }

    /// Pass raw JSON text so epoch/revision u64 values never round through JS Number.
    pub fn query_hosted(&self, epoch_json: &str) -> Result<(), JsValue> {
        let epoch: u64 = serde_json::from_str(epoch_json)
            .map_err(|_| JsValue::from_str("Invalid epoch JSON integer"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::HostedStateQuery { epoch })
                .map_err(|_| JsValue::from_str("Hosted encoding failed"))?,
        )
    }
    pub fn set_hosted_mode(&self, reference_json: &str, mode_json: &str) -> Result<(), JsValue> {
        let reference: HostRevision = serde_json::from_str(reference_json)
            .map_err(|_| JsValue::from_str("Invalid HostRevision"))?;
        let mode: HostedMode =
            serde_json::from_str(mode_json).map_err(|_| JsValue::from_str("Invalid HostedMode"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::HostedSetMode { reference, mode })
                .map_err(|_| JsValue::from_str("Hosted encoding failed"))?,
        )
    }
    pub fn minimize_hosted(&self, reference_json: &str) -> Result<(), JsValue> {
        self.hosted_viewport(reference_json, HostViewportAction::Minimize)
    }
    pub fn restore_hosted(&self, reference_json: &str) -> Result<(), JsValue> {
        self.hosted_viewport(reference_json, HostViewportAction::Restore)
    }
    fn hosted_viewport(
        &self,
        reference_json: &str,
        action: HostViewportAction,
    ) -> Result<(), JsValue> {
        let reference: HostRevision = serde_json::from_str(reference_json)
            .map_err(|_| JsValue::from_str("Invalid HostRevision"))?;
        self.request(
            &serde_json::to_string(&ClientCommand::HostedViewport { reference, action })
                .map_err(|_| JsValue::from_str("Hosted encoding failed"))?,
        )
    }
    /// Versioned consumer-local lookup/data/badge palette API. No Host request,
    /// callback, cursor change or persistence. Safe to call from an event hook;
    /// a conflicting synchronous borrow fails with a fixed error rather than panicking.
    pub fn local_toolkit(&self, request_json: &str) -> Result<String, JsValue> {
        let mut inner = self
            .inner
            .try_borrow_mut()
            .map_err(|_| JsValue::from_str("Local toolkit is busy"))?;
        if inner.disposed || !inner.is_live() {
            return Err(JsValue::from_str("Local toolkit requires a Live client"));
        }
        inner
            .state
            .local_toolkit_json(request_json)
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    pub fn state_json(&self) -> Result<String, JsValue> {
        let inner = self.inner.borrow();
        serde_json::to_string(&serde_json::json!({
            "epoch": inner.state.epoch(), "sequence": inner.state.last_sequence(),
            "users": inner.state.users().values().collect::<Vec<_>>(),
            "hosted": inner.state.hosted(), "screens": inner.state.screens(), "windows": inner.state.windows().values().collect::<Vec<_>>(),
            "capabilities": inner.state.capabilities(), "mode": inner.state.mode(),
            "event_grant": inner.ready.as_ref().map(|ready| &ready.events),
            "control_grant": inner.ready.as_ref().map(|ready| &ready.controls),
            "stream_state": format!("{:?}", inner.state.state())
        })).map_err(|_| JsValue::from_str("Could not encode application state"))
    }

    pub fn dispose(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.disposed {
            return;
        }
        inner.disposed = true;
        inner.stop("Client disposed", true);
        inner.hooks.clear();
        inner.demo_callbacks.clear();
        inner.listeners.clear();
        if let Some(observer) = inner.resize_observer.take() {
            observer.disconnect();
        }
        inner.resize_callback = None;
        inner.event_handler = None;
        inner.notification = None;
    }
}
impl Drop for WebClient {
    fn drop(&mut self) {
        self.dispose();
    }
}

fn element<T: JsCast>(root: &Element, selector: &str) -> Result<T, JsValue> {
    root.query_selector(selector)?
        .ok_or_else(|| JsValue::from_str("Missing demo element"))?
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str("Wrong demo element type"))
}

fn listen(
    inner: &Rc<RefCell<Inner>>,
    target: &EventTarget,
    name: &'static str,
    action: impl Fn(&mut Inner, Event) + 'static,
) -> Result<(), JsValue> {
    let weak = Rc::downgrade(inner);
    let listener = Listener::new(target, name, move |event| {
        if let Some(inner) = weak.upgrade() {
            action(&mut inner.borrow_mut(), event);
            notify(&inner);
        }
    })?;
    inner.borrow_mut().listeners.push(listener);
    Ok(())
}

/// Bounding rectangle and topmost DOM hit both have to agree. Rectangles and
/// DPR are captured for pairing; targets are never retained after cleanup.
fn hit_stamp(
    root: &Element,
    selector: &str,
    canvas: CanvasGeometry,
    revision: u64,
    point: [f64; 2],
) -> Option<HitStamp> {
    canvas.backing_size().ok()?;
    if !point.iter().all(|value| value.is_finite())
        || point[0] < canvas.left
        || point[0] >= canvas.left + canvas.width
        || point[1] < canvas.top
        || point[1] >= canvas.top + canvas.height
    {
        return None;
    }
    let target = root.query_selector(selector).ok()??;
    if target
        .closest("[disabled], [aria-disabled='true'], [inert]")
        .ok()?
        .is_some()
    {
        return None;
    }
    let rect = target.get_bounding_client_rect();
    let target_rect = [rect.left(), rect.top(), rect.width(), rect.height()];
    if !target_rect.iter().all(|value| value.is_finite())
        || rect.width() <= 0.0
        || rect.height() <= 0.0
        || point[0] < rect.left()
        || point[0] >= rect.right()
        || point[1] < rect.top()
        || point[1] >= rect.bottom()
    {
        return None;
    }
    let top = root
        .owner_document()?
        .element_from_point(point[0] as f32, point[1] as f32)?;
    if !target.contains(Some(top.as_ref())) {
        return None;
    }
    Some(HitStamp {
        target,
        target_rect,
        canvas,
        revision,
    })
}

fn notify(inner: &Rc<RefCell<Inner>>) {
    {
        let mut state = inner.borrow_mut();
        if state.dispatching || state.disposed {
            return;
        }
        state.dispatching = true;
    }
    // Reentrant disconnect may enqueue one local notification. A finite budget
    // also bounds a handler that repeatedly disconnects itself.
    for _ in 0..64 {
        let (handler, notification, clicks, ids, generation, layout_revision) = {
            let mut state = inner.borrow_mut();
            (
                state.event_handler.clone(),
                state.notification.take(),
                std::mem::take(&mut state.click_deliveries),
                state.hooks.ids(),
                state.generation,
                state.layout_revision,
            )
        };
        if notification.is_none() && clicks.is_empty() {
            break;
        }
        let parsed = notification
            .as_ref()
            .and_then(|data| serde_json::from_str::<ServerMessage>(data).ok());
        if let (Some(handler), Some(data)) = (handler, &notification) {
            invoke(inner, &handler, data);
        }
        for token in ids {
            let callback = {
                let state = inner.borrow();
                if state.disposed || state.generation != generation {
                    None
                } else if let Some(Hook::Event(hook)) = state.hooks.get(&token) {
                    notification
                        .as_ref()
                        .filter(|_| hook.filter.matches(parsed.as_ref()))
                        .map(|data| (hook.callback.clone(), data.clone()))
                } else {
                    None
                }
            };
            if let Some((callback, payload)) = callback {
                invoke(inner, &callback, &payload);
            }
            for delivery in clicks.iter().filter(|delivery| delivery.token == token) {
                let callback = {
                    let state = inner.borrow();
                    if state.disposed
                        || state.generation != generation
                        || state.layout_revision != layout_revision
                        || !state.is_live()
                    {
                        None
                    } else if let Some(Hook::Click(binding)) = state.hooks.get(&token) {
                        state.click_geometry().ok().and_then(|geometry| {
                            let screens = state.state.screens()?;
                            let stamp = hit_stamp(
                                &state.binding_root,
                                &binding.options.selector,
                                geometry,
                                screens.revision,
                                delivery.point,
                            )?;
                            (stamp == delivery.stamp).then(|| binding.callback.clone())
                        })
                    } else {
                        None
                    }
                };
                if let Some(callback) = callback {
                    invoke(inner, &callback, &delivery.payload);
                }
            }
        }
    }
    let mut state = inner.borrow_mut();
    state.dispatching = false;
    state.notification = None;
    state.click_deliveries.clear();
}

fn invoke(inner: &Rc<RefCell<Inner>>, callback: &js_sys::Function, payload: &str) {
    if callback
        .call1(&JsValue::NULL, &JsValue::from_str(payload))
        .is_err()
    {
        inner
            .borrow_mut()
            .notice("Application callback raised an error");
    }
}

fn open_connection(
    inner_rc: &Rc<RefCell<Inner>>,
    endpoint: String,
    credential: String,
) -> Result<(), &'static str> {
    let mut inner = inner_rc.borrow_mut();
    if inner.disposed || inner.generation == u64::MAX {
        return Err("Client is disposed or dispatch identities exhausted");
    }
    if inner.socket.is_some() {
        return Err("Disconnect the current event connection first");
    }
    let url = Url::new(&endpoint).map_err(|_| "Invalid event endpoint")?;
    if !matches!(url.protocol().as_str(), "ws:" | "wss:")
        || !url.username().is_empty()
        || !url.password().is_empty()
        || !url.search().is_empty()
        || !url.hash().is_empty()
    {
        return Err("Use a WebSocket endpoint without credentials, query or fragment");
    }
    if credential.is_empty() || credential.len() > 4096 || credential.chars().any(char::is_control)
    {
        return Err("Enter the separately approved event-only credential");
    }
    inner.stop("Opening event connection", false);
    inner.socket_listeners.clear();
    inner.state = ClientState::default();
    inner.credential_once = Some(credential);
    let socket = match WebSocket::new(&endpoint) {
        Ok(socket) => socket,
        Err(_) => {
            inner.credential_once = None;
            return Err("Could not open event endpoint");
        }
    };
    let listeners = (|| -> Result<Vec<Listener>, &'static str> {
        let weak: Weak<RefCell<Inner>> = Rc::downgrade(inner_rc);
        let open = Listener::new(socket.as_ref(), "open", move |_| {
            if let Some(inner) = weak.upgrade() {
                let mut inner = inner.borrow_mut();
                let Some(credential) = inner.credential_once.take() else {
                    inner.stop("Event credential unavailable", false);
                    return;
                };
                let hello = EventHello {
                    version: PROTOCOL_VERSION,
                    credential,
                };
                let sent = serde_json::to_string(&hello)
                    .ok()
                    .and_then(|json| inner.socket.as_ref()?.send_with_str(&json).ok());
                if sent.is_none() {
                    inner.stop("Event authentication send failed", false);
                } else {
                    inner.notice("Awaiting host event approval");
                }
            }
        })
        .map_err(|_| "Could not register socket lifecycle")?;
        let weak = Rc::downgrade(inner_rc);
        let message = Listener::new(socket.as_ref(), "message", move |event| {
            if let Some(inner_rc) = weak.upgrade() {
                let mut inner = inner_rc.borrow_mut();
                let result = event
                    .dyn_into::<MessageEvent>()
                    .ok()
                    .and_then(|event| event.data().as_string());
                match result {
                    Some(data) => {
                        if let Err(error) = inner.receive(&data) {
                            inner.stop(error, false);
                        }
                    }
                    None => inner.stop("Only bounded text event frames are supported", false),
                }
                drop(inner);
                notify(&inner_rc);
            }
        })
        .map_err(|_| "Could not register socket lifecycle")?;
        let weak = Rc::downgrade(inner_rc);
        let close = Listener::new(socket.as_ref(), "close", move |_| {
            if let Some(inner) = weak.upgrade() {
                inner
                    .borrow_mut()
                    .stop("Event connection closed; reconnect explicitly", false);
                notify(&inner);
            }
        })
        .map_err(|_| "Could not register socket lifecycle")?;
        let weak = Rc::downgrade(inner_rc);
        let error = Listener::new(socket.as_ref(), "error", move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.borrow_mut().stop(
                    "Event connection failed; verify endpoint, exact Origin and event credential",
                    false,
                );
                notify(&inner);
            }
        })
        .map_err(|_| "Could not register socket lifecycle")?;
        Ok(vec![open, message, close, error])
    })();
    inner.socket_listeners = match listeners {
        Ok(listeners) => listeners,
        Err(error) => {
            inner.credential_once = None;
            let _ = socket.close();
            return Err(error);
        }
    };
    inner.socket = Some(socket);
    inner.connect_button.set_disabled(true);
    Ok(())
}

/// Builds a bounded example entirely in Rust. Connecting, selecting and enabling
/// are separate user actions; mount alone sends no network or input events.
#[wasm_bindgen]
pub fn mount_demo(container_id: &str) -> Result<WebClient, JsValue> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("Document unavailable"))?;
    let root = document
        .get_element_by_id(container_id)
        .ok_or_else(|| JsValue::from_str("Demo container unavailable"))?;
    root.set_inner_html(r#"
      <h1>Cooperative virtual input</h1>
      <p>Observe two host-created users on one canvas. Pointer and keyboard controls require separate host approval. Physical input, pen, touch and target injection are unavailable.</p>
      <label>Event endpoint <input data-endpoint type="url" value="ws://127.0.0.1:49791/events" autocomplete="off"></label>
      <label>Event-only credential <input data-credential type="password" autocomplete="off" spellcheck="false"></label>
      <button data-connect>Connect</button><button data-disconnect>Disconnect</button><button data-refresh disabled>Refresh subscription</button>
      <label>User <select data-users><option value="">Connect, then choose a user</option></select></label>
      <label><input data-pointer type="checkbox" disabled>Enable canvas pointer</label>
      <label><input data-keys type="checkbox" disabled>Enable focused canvas keyboard</label>
      <p data-status role="status" aria-live="polite">Disconnected. Event export and all input are off.</p>
      <canvas data-canvas tabindex="0" aria-label="Cooperative user canvas" style="display:block;width:100%;height:480px;border:0;padding:0;touch-action:auto;background:#111b2b"></canvas>
      <p data-event-observation>Reusable event listener: awaiting a validated frame.</p>
      <p data-click-observation>Virtual canvas binding: awaiting an authorized press/release pair.</p>
      <p>Choose a user, enable an approved control, then focus the canvas. Tab leaves the canvas. Blur releases this client's held keys and buttons. Refresh and reconnect never replay local input.</p>
    "#);
    let canvas: HtmlCanvasElement = element(&root, "[data-canvas]")?;
    let context = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("Canvas 2D unavailable"))?
        .dyn_into::<CanvasRenderingContext2d>()?;
    let inner = Rc::new(RefCell::new(Inner {
        canvas: canvas.clone(),
        context,
        status: element(&root, "[data-status]")?,
        users: element(&root, "[data-users]")?,
        pointer_toggle: element(&root, "[data-pointer]")?,
        key_toggle: element(&root, "[data-keys]")?,
        endpoint: element(&root, "[data-endpoint]")?,
        credential: element(&root, "[data-credential]")?,
        connect_button: element(&root, "[data-connect]")?,
        refresh_button: element(&root, "[data-refresh]")?,
        state: ClientState::default(),
        local: LocalInput::default(),
        ready: None,
        socket: None,
        subscription: None,
        pending: BTreeSet::new(),
        pending_hosted: BTreeMap::new(),
        hosted_receipt: mux_application_sdk::client::HostedReceipt::default(),
        pending_windows: BTreeMap::new(),
        next_request: 0,
        authenticated: false,
        awaiting_subscription: false,
        credential_once: None,
        listeners: Vec::new(),
        socket_listeners: Vec::new(),
        resize_observer: None,
        resize_callback: None,
        event_handler: None,
        notification: None,
        binding_root: root.clone(),
        hooks: Registry::default(),
        click_deliveries: Vec::new(),
        dispatching: false,
        generation: 0,
        disposed: false,
        layout_revision: 0,
        demo_callbacks: Vec::new(),
    }));
    let connect_button: HtmlButtonElement = element(&root, "[data-connect]")?;
    let weak = Rc::downgrade(&inner);
    let listener = Listener::new(connect_button.as_ref(), "click", move |_| {
        if let Some(inner) = weak.upgrade() {
            let (endpoint, credential) = {
                let inner = inner.borrow();
                let values = (inner.endpoint.value(), inner.credential.value());
                inner.credential.set_value("");
                values
            };
            if let Err(error) = open_connection(&inner, endpoint, credential) {
                inner.borrow_mut().stop(error, false);
            }
            notify(&inner);
        }
    })?;
    inner.borrow_mut().listeners.push(listener);
    let disconnect: HtmlButtonElement = element(&root, "[data-disconnect]")?;
    listen(&inner, disconnect.as_ref(), "click", |inner, _| {
        inner.stop("Disconnected; reconnect explicitly", true)
    })?;
    let refresh: HtmlButtonElement = element(&root, "[data-refresh]")?;
    listen(&inner, refresh.as_ref(), "click", |inner, _| {
        if let Err(error) = inner.refresh() {
            inner.stop(error, false);
        }
    })?;
    let users: HtmlSelectElement = element(&root, "[data-users]")?;
    listen(&inner, users.as_ref(), "change", |inner, _| {
        let selected = inner.users.value();
        inner.release(true);
        inner.disable_inputs();
        inner.local.selected = inner
            .state
            .users()
            .keys()
            .copied()
            .find(|id| format!("{}:{}", id.epoch, id.local) == selected);
        inner.users.set_value(&selected);
        inner.update_controls();
    })?;
    let pointer: HtmlInputElement = element(&root, "[data-pointer]")?;
    listen(&inner, pointer.as_ref(), "change", |inner, _| {
        inner.release(true);
        inner.local.pointer_enabled =
            !inner.pointer_toggle.disabled() && inner.pointer_toggle.checked();
    })?;
    let keys: HtmlInputElement = element(&root, "[data-keys]")?;
    listen(&inner, keys.as_ref(), "change", |inner, _| {
        inner.release(true);
        inner.local.keys_enabled = !inner.key_toggle.disabled() && inner.key_toggle.checked();
    })?;
    listen(&inner, canvas.as_ref(), "pointermove", |inner, event| {
        if !inner.local.pointer_enabled {
            return;
        }
        if let Ok(event) = event.dyn_into::<PointerEvent>()
            && inner.current_user(false).is_ok()
        {
            match inner.motion(&event) {
                Ok(()) => event.prevent_default(),
                Err(error) => inner.notice(error),
            }
        }
    })?;
    for (name, transition) in [
        ("pointerdown", Transition::Down),
        ("pointerup", Transition::Up),
    ] {
        listen(&inner, canvas.as_ref(), name, move |inner, event| {
            if !inner.local.pointer_enabled {
                return;
            }
            let Ok(event) = event.dyn_into::<PointerEvent>() else {
                return;
            };
            if event.pointer_type() != "mouse" {
                inner.notice("Pen and touch input are unavailable");
                return;
            }
            let Ok(user) = inner.current_user(false) else {
                return;
            };
            let Some(button) = mouse_button(event.button()) else {
                return;
            };
            let held = inner.local.held_buttons().contains(&(user, button));
            if (transition == Transition::Down) == held {
                return;
            }
            // Button release remains valid outside the captured canvas; only
            // motion needs a point inside the mapped desktop region.
            if transition == Transition::Down {
                if let Err(error) = inner.motion(&event) {
                    inner.notice(error);
                    return;
                }
                let _ = inner.canvas.focus();
                let _ = inner.canvas.set_pointer_capture(event.pointer_id());
            }
            if let Err(error) = inner.input(user, Input::Button { button, transition }) {
                inner.stop(error, false);
            }
            if transition == Transition::Up && inner.local.held_buttons().is_empty() {
                let _ = inner.canvas.release_pointer_capture(event.pointer_id());
            }
            event.prevent_default();
        })?;
    }
    for name in ["blur", "pointercancel", "lostpointercapture"] {
        listen(&inner, canvas.as_ref(), name, |inner, _| {
            inner.release(true)
        })?;
    }
    listen(&inner, canvas.as_ref(), "contextmenu", |inner, event| {
        if inner.current_user(false).is_ok() {
            event.prevent_default();
        }
    })?;
    listen(&inner, canvas.as_ref(), "wheel", |inner, event| {
        let Ok(user) = inner.current_user(false) else {
            return;
        };
        let Ok(event) = event.dyn_into::<WheelEvent>() else {
            return;
        };
        // DOM deltas are normalized explicitly. Positive vertical desktop wheel
        // means up; positive horizontal means right, matching the core contract.
        let factor = match event.delta_mode() {
            0 => 1.0,
            1 => 40.0,
            2 => 480.0,
            _ => {
                inner.notice("Unsupported wheel delta mode");
                return;
            }
        };
        for (axis, delta) in [
            (WheelAxis::Vertical, -event.delta_y()),
            (WheelAxis::Horizontal, event.delta_x()),
        ] {
            let units = (delta * factor).round().clamp(-12000.0, 12000.0) as i32;
            if units != 0
                && let Err(error) = inner.input(user, Input::Wheel { axis, units })
            {
                inner.stop(error, false);
                return;
            }
        }
        event.prevent_default();
    })?;
    for (name, transition) in [("keydown", Transition::Down), ("keyup", Transition::Up)] {
        listen(&inner, canvas.as_ref(), name, move |inner, event| {
            let Ok(user) = inner.current_user(true) else {
                return;
            };
            let Ok(event) = event.dyn_into::<KeyboardEvent>() else {
                return;
            };
            if event.code() == "Tab" {
                inner.release(true);
                return;
            }
            if event.repeat() || event.is_composing() {
                return;
            }
            let Some(key) = hid_key(&event.code()) else {
                inner.notice("This browser key code is unavailable in the virtual HID mapping");
                return;
            };
            let held = inner.local.held_keys().contains(&(user, key));
            if (transition == Transition::Down) == held {
                return;
            }
            if let Err(error) = inner.input(user, Input::Key { key, transition }) {
                inner.stop(error, false);
            }
            event.prevent_default();
        })?;
    }
    // Layout invalidation only clears virtual pairing. These listeners never
    // produce input or alter grants.
    if let Some(window) = web_sys::window() {
        for name in ["resize", "scroll"] {
            listen(&inner, window.as_ref(), name, |inner, _| {
                inner.clear_clicks()
            })?;
        }
    }
    // Window blur only performs cleanup; no global input production is captured.
    if let Some(window) = web_sys::window() {
        listen(&inner, window.as_ref(), "blur", |inner, _| {
            inner.release(true)
        })?;
        listen(&inner, window.as_ref(), "pagehide", |inner, _| {
            inner.stop("Page hidden; reconnect explicitly", true)
        })?;
    }
    let weak = Rc::downgrade(&inner);
    let callback = Closure::wrap(Box::new(move |_: js_sys::Array, _: ResizeObserver| {
        if let Some(inner) = weak.upgrade() {
            let mut inner = inner.borrow_mut();
            inner.clear_clicks();
            if let Err(error) = inner.draw() {
                inner.notice(error);
            }
        }
    }) as Box<dyn FnMut(js_sys::Array, ResizeObserver)>);
    let observer = ResizeObserver::new(callback.as_ref().unchecked_ref())?;
    observer.observe(&canvas);
    inner.borrow_mut().resize_observer = Some(observer);
    inner.borrow_mut().resize_callback = Some(callback);
    inner.borrow().draw().map_err(JsValue::from_str)?;
    let client = WebClient { inner };
    // Original Rust examples of the same public reusable API. They only update
    // informational text; neither callback activates a DOM control or sends input.
    let event_output: HtmlElement = element(&root, "[data-event-observation]")?;
    let events = Closure::wrap(Box::new(move |_: String| {
        event_output.set_text_content(Some(
            "Reusable event listener: validated notification received.",
        ));
    }) as Box<dyn FnMut(String)>);
    client.add_event_listener(
        r#"{"kind":"any"}"#,
        events.as_ref().unchecked_ref::<js_sys::Function>().clone(),
    )?;
    let click_output: HtmlElement = element(&root, "[data-click-observation]")?;
    let clicks = Closure::wrap(Box::new(move |_: String| {
        click_output.set_text_content(Some(
            "Virtual canvas binding: authorized press/release pair observed.",
        ));
    }) as Box<dyn FnMut(String)>);
    client.bind_virtual_click(
        r#"{"selector":"[data-canvas]","button":"primary"}"#,
        clicks.as_ref().unchecked_ref::<js_sys::Function>().clone(),
    )?;
    client
        .inner
        .borrow_mut()
        .demo_callbacks
        .extend([events, clicks]);
    Ok(client)
}
