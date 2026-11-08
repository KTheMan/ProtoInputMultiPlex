//! Owner-thread-only native bindings. Local engine IDs are never GUI IDs.
use super::ClientError;
use crate::{
    Engine, Grant, RecipientId, ScreenState, UserScope, WindowControl, WindowId,
    WindowInfo, WindowObservation,
};
use mux_core::desktop::Bounds;
use mux_windows::application_window::WindowAdapter;
use std::collections::BTreeMap;

pub(super) const MAX_OWNED_WINDOWS: usize = 16;

pub(super) struct OwnedWindows {
    engine: Engine,
    adapter: WindowAdapter,
    recipient: RecipientId,
    entries: BTreeMap<u64, Entry>,
    next_token: u64,
}

struct Entry {
    local: WindowId,
    generation: u64,
    remote: Option<WindowId>,
    last: Option<WindowObservation>,
}

fn native_error(error: crate::Error) -> ClientError {
    match error {
        crate::Error::Limit => ClientError::Limit,
        crate::Error::Denied => ClientError::Denied,
        crate::Error::MissingWindow => ClientError::Window,
        _ => ClientError::Window,
    }
}

pub(super) fn observation(info: WindowInfo) -> WindowObservation {
    WindowObservation {
        bounds: info.bounds,
        metadata: info.metadata,
        visible: info.visible,
        minimized: info.minimized,
        maximized: info.maximized,
        click_through: info.click_through,
        enabled: info.enabled,
        topmost: info.topmost,
        opacity: info.opacity,
    }
}

impl OwnedWindows {
    pub(super) fn new() -> Result<Self, ClientError> {
        // The private engine exists only to reuse the reviewed, owner-thread
        // WindowAdapter registration and action path. No local ID is sent on wire.
        let desktop = Bounds {
            left: -1_000_000,
            top: -1_000_000,
            right: 1_000_000,
            bottom: 1_000_000,
        };
        let mut engine = Engine::new(
            1,
            ScreenState {
                revision: 1,
                desktop,
                screens: vec![desktop],
            },
        )
        .map_err(native_error)?;
        let recipient = engine
            .register_recipient("native-window-owner")
            .map_err(native_error)?;
        engine
            .approve_grant(
                recipient,
                Grant {
                    users: UserScope::Only(Default::default()),
                    pointer_events: false,
                    key_events: false,
                    owned_windows: true,
                },
            )
            .map_err(native_error)?;
        Ok(Self {
            engine,
            adapter: WindowAdapter::new().map_err(native_error)?,
            recipient,
            entries: BTreeMap::new(),
            next_token: 1,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn register(
        &mut self,
        hwnd: usize,
    ) -> Result<(u64, u64, WindowObservation), ClientError> {
        if self.entries.len() >= MAX_OWNED_WINDOWS {
            return Err(ClientError::Limit);
        }
        let token = self.next_token;
        self.next_token = token.checked_add(1).ok_or(ClientError::Limit)?;
        let local = self
            .adapter
            .register(&mut self.engine, self.recipient, hwnd)
            .map_err(native_error)?;
        let observed = match self.adapter.refresh(&mut self.engine, local) {
            Ok(info) => observation(info),
            Err(error) => {
                let _ = self.adapter.retire(&mut self.engine, local);
                return Err(native_error(error));
            }
        };
        self.entries.insert(
            token,
            Entry {
                local,
                generation: 1,
                remote: None,
                last: Some(observed.clone()),
            },
        );
        Ok((token, 1, observed))
    }

    pub(super) fn bind_remote(&mut self, token: u64, window: WindowId) -> Result<(), ClientError> {
        if self.entries.values().any(|other| other.remote == Some(window)) {
            return Err(ClientError::Protocol);
        }
        let entry = self.entries.get_mut(&token).ok_or(ClientError::Window)?;
        if entry.remote.is_some() {
            return Err(ClientError::Protocol);
        }
        entry.remote = Some(window);
        Ok(())
    }

    pub(super) fn refresh(
        &mut self,
        token: u64,
    ) -> Result<(WindowId, u64, WindowObservation), ClientError> {
        let entry = self.entries.get(&token).ok_or(ClientError::Window)?;
        let window = entry.remote.ok_or(ClientError::Busy)?;
        let generation = entry.generation;
        let local = entry.local;
        let info = self
            .adapter
            .refresh(&mut self.engine, local)
            .map_err(native_error)?;
        let observed = observation(info);
        self.entries.get_mut(&token).ok_or(ClientError::Window)?.last = Some(observed.clone());
        Ok((window, generation, observed))
    }

    pub(super) fn notify_changed(
        &mut self,
        token: u64,
    ) -> Result<Option<(WindowId, u64, WindowObservation)>, ClientError> {
        let entry = self.entries.get(&token).ok_or(ClientError::Window)?;
        let Some(window) = entry.remote else {
            return Ok(None);
        };
        let local = entry.local;
        let generation = entry.generation;
        let observed = observation(
            self.adapter
                .refresh(&mut self.engine, local)
                .map_err(native_error)?,
        );
        let entry = self.entries.get_mut(&token).ok_or(ClientError::Window)?;
        if entry.last.as_ref() == Some(&observed) {
            return Ok(None);
        }
        Ok(Some((window, generation, observed)))
    }

    pub(super) fn mark_reported(&mut self, token: u64, observed: WindowObservation) {
        if let Some(entry) = self.entries.get_mut(&token) {
            entry.last = Some(observed);
        }
    }

    pub(super) fn invalidate_report(&mut self, token: u64) {
        if let Some(entry) = self.entries.get_mut(&token) {
            entry.last = None;
        }
    }

    pub(super) fn retire(
        &mut self,
        token: u64,
    ) -> Result<(Option<WindowId>, u64, Result<(), ClientError>), ClientError> {
        let entry = self.entries.remove(&token).ok_or(ClientError::Window)?;
        let local_result = self.adapter
            .retire(&mut self.engine, entry.local)
            .map_err(native_error);
        Ok((entry.remote, entry.generation, local_result))
    }

    pub(super) fn apply(
        &mut self,
        window: WindowId,
        token: u64,
        generation: u64,
        control: WindowControl,
    ) -> Result<WindowObservation, crate::Error> {
        let entry = self.entries.get(&token).ok_or(crate::Error::MissingWindow)?;
        if entry.generation != generation || entry.remote != Some(window) {
            return Err(crate::Error::Denied);
        }
        let local = entry.local;
        // Engine checks action/bounds/ownership; WindowAdapter rechecks HWND,
        // creating thread, process, cookie and DPI before and after Win32 calls.
        let observed = self.engine
            .control_window(self.recipient, local, control, &mut self.adapter)
            .map(|ack| observation(ack.observed))?;
        self.entries.get_mut(&token).ok_or(crate::Error::MissingWindow)?.last = Some(observed.clone());
        Ok(observed)
    }

    pub(super) fn clear(&mut self) {
        let tokens: Vec<u64> = self.entries.keys().copied().collect();
        for token in tokens {
            let _ = self.retire(token);
        }
    }
}
