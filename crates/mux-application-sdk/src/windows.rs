use crate::host::WindowBackend;
use mux_core::application_event::*;

/// No native backend is initialized until an explicit local window registration.
#[derive(Default)]
pub struct PlatformWindows {
    #[cfg(all(windows, feature = "native-windows"))]
    adapter: Option<mux_windows::application_window::WindowAdapter>,
}

impl OwnWindowAdapter for PlatformWindows {
    fn supported(&self, window: WindowId, action: WindowAction) -> bool {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_ref()
                .is_some_and(|adapter| adapter.supported(window, action))
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = (window, action);
            false
        }
    }

    fn apply(
        &mut self,
        owner: RecipientId,
        current: &WindowInfo,
        control: &WindowControl,
    ) -> Result<WindowInfo, Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_mut()
                .ok_or(Error::MissingWindow)?
                .apply(owner, current, control)
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = (owner, current, control);
            Err(unavailable())
        }
    }
}

impl WindowBackend for PlatformWindows {
    fn register(
        &mut self,
        engine: &mut Engine,
        recipient: RecipientId,
        native_window: usize,
    ) -> Result<WindowId, Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            if self.adapter.is_none() {
                self.adapter = Some(mux_windows::application_window::WindowAdapter::new()?);
            }
            self.adapter.as_mut().ok_or(Error::MissingWindow)?.register(
                engine,
                recipient,
                native_window,
            )
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = (engine, recipient, native_window);
            Err(unavailable())
        }
    }
    fn refresh(&self, engine: &mut Engine, window: WindowId) -> Result<WindowInfo, Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_ref()
                .ok_or(Error::MissingWindow)?
                .refresh(engine, window)
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = (engine, window);
            Err(unavailable())
        }
    }
    fn retire(&mut self, engine: &mut Engine, window: WindowId) -> Result<(), Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_mut()
                .ok_or(Error::MissingWindow)?
                .retire(engine, window)
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = (engine, window);
            Err(unavailable())
        }
    }
    fn notify(&self, window: WindowId) -> Result<(), Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_ref()
                .ok_or(Error::MissingWindow)?
                .notify(window)
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = window;
            Err(unavailable())
        }
    }
    fn acknowledge(&self, window: WindowId) -> Result<(), Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            self.adapter
                .as_ref()
                .ok_or(Error::MissingWindow)?
                .acknowledge_wake(window)
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            let _ = window;
            Err(unavailable())
        }
    }
    fn wake_message(&self) -> Result<u32, Error> {
        #[cfg(all(windows, feature = "native-windows"))]
        {
            Ok(self
                .adapter
                .as_ref()
                .ok_or(Error::MissingWindow)?
                .wake_message())
        }
        #[cfg(not(all(windows, feature = "native-windows")))]
        {
            Err(unavailable())
        }
    }
}

#[cfg(not(all(windows, feature = "native-windows")))]
fn unavailable() -> Error {
    Error::Unsupported("native own-window backend unavailable on this build")
}
