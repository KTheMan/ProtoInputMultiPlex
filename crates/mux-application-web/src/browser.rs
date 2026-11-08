use crate::geometry::CanvasGeometry;
use crate::local_input::{LocalInput, hid_key, mouse_button, selects};
use mux_application_sdk::*;
use std::cell::RefCell;
use std::collections::BTreeSet;
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
}

impl Inner {
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
        socket
            .send_with_str(&json)
            .map_err(|_| "Event socket send failed")?;
        self.next_request = request_id;
        self.pending.insert(request_id);
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
        self.notice(message);
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
        if self.awaiting_subscription {
            return Err("A fresh subscription snapshot is already pending");
        }
        self.release(true);
        self.disable_inputs();
        self.state.disconnect();
        let capacity = self
            .ready
            .as_ref()
            .map_or(256, |ready| ready.limits.max_queue.min(256));
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
        match message {
            ServerMessage::Ready { ready } => {
                if self.authenticated
                    || ready.version != PROTOCOL_VERSION
                    || ready.epoch == 0
                    || ready.limits.max_request_bytes == 0
                    || ready.limits.max_poll_events == 0
                    || ready.limits.max_poll_events > MAX_POLL_EVENTS
                    || ready.limits.max_queue == 0
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
                match reply {
                    Reply::Ok { version, response } => {
                        if version != PROTOCOL_VERSION {
                            return Err("Response version mismatch");
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
                                self.subscription = Some(subscription);
                                self.awaiting_subscription = false;
                                self.notice("Live virtual events. Select a user and explicitly enable controls");
                                self.rebuild_users()?;
                            }
                            ClientResponse::Snapshot { .. } => {
                                self.notice("Snapshot query acknowledged; subscribe/refresh establishes the live stream");
                            }
                            ClientResponse::Polled { .. } => {
                                return Err("Unexpected poll response on server-pushed stream");
                            }
                            ClientResponse::Disconnected => {
                                self.stop("Server disconnected; reconnect explicitly", false);
                            }
                            _ => { /* Acknowledgements do not mutate authoritative rendering. */ }
                        }
                    }
                    Reply::Error { version, error } => {
                        if version != PROTOCOL_VERSION {
                            return Err("Response version mismatch");
                        }
                        // Any rejected input can invalidate locally predicted holds.
                        // Close and let the server retire this controller's holds.
                        let notice =
                            format!("Request rejected ({:?}); reconnect explicitly", error.code);
                        self.stop(&notice, false);
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
                    self.release(true);
                }
                match self
                    .state
                    .fold_poll(stream)
                    .map_err(|_| "Invalid ordered event stream; reconnect explicitly")?
                {
                    FoldOutcome::Applied(_) => {
                        self.rebuild_users()?;
                    }
                    FoldOutcome::NeedsSnapshot => {
                        self.release(false);
                        self.disable_inputs();
                        self.notice("Sequence gap: state cleared. Use Refresh subscription for a fresh snapshot");
                    }
                    FoldOutcome::Ended(_) => {
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
        }
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
        self.inner.borrow_mut().event_handler = handler;
    }

    pub fn refresh_subscription(&self) -> Result<(), JsValue> {
        self.inner.borrow_mut().refresh().map_err(JsValue::from_str)
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
        let input = match &command {
            ClientCommand::Input { user, input }
            | ClientCommand::InputAtRevision { user, input, .. } => Some((*user, *input)),
            _ => None,
        };
        if let Some((user, input)) = input {
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
                || if input.is_key() {
                    !ready.controls.key_input
                } else {
                    !ready.controls.pointer_input
                }
            {
                return Err(JsValue::from_str("Input is outside the current grant"));
            }
            inner.send(command).map_err(JsValue::from_str)?;
            inner.local.record(user, input);
            Ok(())
        } else {
            inner.send(command).map_err(JsValue::from_str)
        }
    }

    pub fn state_json(&self) -> Result<String, JsValue> {
        let inner = self.inner.borrow();
        serde_json::to_string(&serde_json::json!({
            "epoch": inner.state.epoch(), "sequence": inner.state.last_sequence(),
            "users": inner.state.users().values().collect::<Vec<_>>(),
            "screens": inner.state.screens(), "windows": inner.state.windows().values().collect::<Vec<_>>(),
            "capabilities": inner.state.capabilities(), "mode": inner.state.mode(),
            "event_grant": inner.ready.as_ref().map(|ready| &ready.events),
            "control_grant": inner.ready.as_ref().map(|ready| &ready.controls),
            "stream_state": format!("{:?}", inner.state.state())
        })).map_err(|_| JsValue::from_str("Could not encode application state"))
    }

    pub fn dispose(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.stop("Client disposed", true);
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

fn notify(inner: &Rc<RefCell<Inner>>) {
    let (handler, notification) = {
        let mut inner = inner.borrow_mut();
        (inner.event_handler.clone(), inner.notification.take())
    };
    if let (Some(handler), Some(notification)) = (handler, notification)
        && handler
            .call1(&JsValue::NULL, &JsValue::from_str(&notification))
            .is_err()
    {
        inner
            .borrow_mut()
            .notice("Application event handler raised an error");
    }
}

fn open_connection(
    inner_rc: &Rc<RefCell<Inner>>,
    endpoint: String,
    credential: String,
) -> Result<(), &'static str> {
    let mut inner = inner_rc.borrow_mut();
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
    Ok(WebClient { inner })
}
