//! Additive native-client ABI. These tokens are not standalone Host tokens.
use super::*;
use crate::abi_contract::Status;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    mem::ManuallyDrop,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::AtomicU64,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Entry {
    client: NativeClient,
    response: Option<Vec<u8>>,
    response_after_stop: bool,
    poisoned: bool,
}
// No registered DLL TLS destructor may outlive explicit successful destruction.
// The owner MUST remove every entry before thread exit. Violating that contract
// leaks the map/client and forfeits DLL unload permission for process lifetime.
// Implicit TLS detach cannot safely join workers and must never pretend it did.
thread_local! { static CLIENTS: RefCell<ManuallyDrop<BTreeMap<u64, Entry>>> = const { RefCell::new(ManuallyDrop::new(BTreeMap::new())) }; }
fn status(error: ClientError) -> i32 {
    (match error {
        ClientError::Busy => Status::Busy,
        ClientError::Limit => Status::Limit,
        _ => Status::InvalidArgument,
    }) as i32
}
fn boundary(handle: Option<u64>, action: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(code) => code,
        Err(_) => {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                CLIENTS.with(|clients| {
                    if let Ok(mut clients) = clients.try_borrow_mut()
                        && let Some(handle) = handle
                        && let Some(entry) = clients.get_mut(&handle)
                    {
                        entry.poisoned = true;
                        entry.response = None;
                        entry.client.stop();
                    }
                })
            }));
            Status::Panic as i32
        }
    }
}
fn with(handle: u64, action: impl FnOnce(&mut Entry) -> i32) -> i32 {
    CLIENTS.with(|clients| {
        let Ok(mut clients) = clients.try_borrow_mut() else {
            return Status::Busy as i32;
        };
        let Some(entry) = clients.get_mut(&handle) else {
            return Status::InvalidHandle as i32;
        };
        if entry.poisoned {
            return Status::Poisoned as i32;
        }
        entry.client.observe_terminal();
        if entry.client.terminal.is_some() && !entry.response_after_stop {
            entry.response = None;
        }
        action(entry)
    })
}
unsafe fn decode<T: serde::de::DeserializeOwned>(
    input: *const u8,
    length: usize,
) -> Result<T, i32> {
    if input.is_null() || length == 0 || length > MAX_REQUEST_BYTES {
        return Err(Status::InvalidArgument as i32);
    }
    // SAFETY: caller supplies a readable, live allocation of exactly the bounded length.
    let bytes = unsafe { std::slice::from_raw_parts(input, length) };
    let text = std::str::from_utf8(bytes).map_err(|_| Status::InvalidArgument as i32)?;
    serde_json::from_str(text).map_err(|_| Status::InvalidArgument as i32)
}
fn retain(entry: &mut Entry, value: &impl Serialize) -> i32 {
    match serde_json::to_vec(value) {
        Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => {
            entry.response_after_stop = entry.client.terminal.is_some();
            entry.response = Some(bytes);
            Status::Ok as i32
        }
        _ => {
            entry.client.stop();
            entry.response = None;
            Status::Limit as i32
        }
    }
}
/// Explicit connect only. Config is transient UTF-8 JSON; no pointer is retained.
/// # Safety
/// Input must be readable for length (1..=65536), handle_out aligned/writable u64,
/// wake_message_out aligned/writable u32, and allocations disjoint. own_hwnd must be a live top-level HWND on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_connect(
    own_hwnd: usize,
    config: *const u8,
    length: usize,
    handle_out: *mut u64,
    wake_message_out: *mut u32,
) -> i32 {
    boundary(None, || {
        if handle_out.is_null()
            || !handle_out.is_aligned()
            || wake_message_out.is_null()
            || !wake_message_out.is_aligned()
        {
            return Status::InvalidArgument as i32;
        }
        let config: ConnectConfig = match unsafe { decode(config, length) } {
            Ok(c) => c,
            Err(s) => return s,
        };
        CLIENTS.with(|clients| {
            let Ok(mut clients) = clients.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            if clients.len() >= MAX_NATIVE_CLIENTS {
                return Status::Limit as i32;
            }
            let handle = match NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            {
                Ok(h) => h,
                Err(_) => return Status::Limit as i32,
            };
            let client = match NativeClient::connect(config, own_hwnd) {
                Ok(c) => c,
                Err(e) => return status(e),
            };
            let wake_message = client.binding.target.message;
            clients.insert(
                handle,
                Entry {
                    client,
                    response: None,
                    response_after_stop: false,
                    poisoned: false,
                },
            );
            // SAFETY: caller supplies validated scalar allocation.
            unsafe {
                handle_out.write(handle);
                wake_message_out.write(wake_message);
            }
            Status::Ok as i32
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_enable_input(handle: u64, pointer: u32, keys: u32) -> i32 {
    boundary(Some(handle), || {
        if pointer > 1 || keys > 1 {
            return Status::InvalidArgument as i32;
        }
        with(handle, |e| {
            match e.client.set_input_enabled(pointer != 0, keys != 0) {
                Ok(()) => Status::Ok as i32,
                Err(e) => status(e),
            }
        })
    })
}
/// Queues exactly once; OK means local admission, never remote application.
/// # Safety
/// Request is readable UTF-8 JSON, 1..=65536 bytes. request_id_out is aligned,
/// writable, disjoint u64. All allocations remain valid until return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_request(
    handle: u64,
    request: *const u8,
    length: usize,
    request_id_out: *mut u64,
) -> i32 {
    boundary(Some(handle), || {
        if request_id_out.is_null() || !request_id_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        let request: ClientRequest = match unsafe { decode(request, length) } {
            Ok(r) => r,
            Err(s) => return s,
        };
        with(handle, |entry| match entry.client.request(request) {
            Ok(id) => {
                unsafe {
                    request_id_out.write(id);
                }
                Status::Ok as i32
            }
            Err(e) => status(e),
        })
    })
}

/// Register one of this UI thread's own top-level windows. The returned token
/// is local; a GUI WindowId arrives only in a later OwnerResponse.
/// # Safety
/// token_out is aligned, writable and disjoint from SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_window_register(
    handle: u64,
    own_hwnd: usize,
    identity_metadata_consent: u32,
    token_out: *mut u64,
) -> i32 {
    boundary(Some(handle), || {
        if token_out.is_null() || !token_out.is_aligned() || identity_metadata_consent > 1 {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| {
            match entry
                .client
                .register_own_window(own_hwnd, identity_metadata_consent != 0)
            {
                Ok(token) => {
                    unsafe {
                        token_out.write(token);
                    }
                    Status::Ok as i32
                }
                Err(error) => status(error),
            }
        })
    })
}

/// Explicit owner-thread observation. OK means only that a report was queued.
/// # Safety
/// request_id_out is aligned, writable and disjoint from SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_window_refresh(
    handle: u64,
    token: u64,
    identity_metadata_consent: u32,
    request_id_out: *mut u64,
) -> i32 {
    boundary(Some(handle), || {
        if request_id_out.is_null() || !request_id_out.is_aligned() || identity_metadata_consent > 1
        {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| {
            match entry
                .client
                .refresh_own_window(token, identity_metadata_consent != 0)
            {
                Ok(id) => {
                    unsafe {
                        request_id_out.write(id);
                    }
                    Status::Ok as i32
                }
                Err(error) => status(error),
            }
        })
    })
}

/// Call after an owner WndProc state change. Returns request_id 0 if the actual
/// observed state is unchanged; no synthetic timestamp-only update is sent.
/// # Safety
/// request_id_out is aligned, writable and disjoint from SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_window_notify(
    handle: u64,
    token: u64,
    identity_metadata_consent: u32,
    request_id_out: *mut u64,
) -> i32 {
    boundary(Some(handle), || {
        if request_id_out.is_null() || !request_id_out.is_aligned() || identity_metadata_consent > 1
        {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| {
            match entry
                .client
                .notify_own_window_changed(token, identity_metadata_consent != 0)
            {
                Ok(id) => {
                    unsafe {
                        request_id_out.write(id.unwrap_or(0));
                    }
                    Status::Ok as i32
                }
                Err(error) => status(error),
            }
        })
    })
}

/// Retire locally from WM_NCDESTROY before HWND reuse. Remote completion is
/// separate; a lost receipt remains uncertain and is never replayed.
/// # Safety
/// request_id_out is aligned, writable and disjoint from SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_window_retire(
    handle: u64,
    token: u64,
    request_id_out: *mut u64,
) -> i32 {
    boundary(Some(handle), || {
        if request_id_out.is_null() || !request_id_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| {
            match entry.client.retire_own_window(token) {
                Ok(id) => {
                    unsafe {
                        request_id_out.write(id.unwrap_or(0));
                    }
                    Status::Ok as i32
                }
                Err(error) => status(error),
            }
        })
    })
}
/// Drain one validated ServerMessage (JSON null when empty). Retains the output;
/// size/read/discard it before another drain/query. On a wake drain until null.
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_drain(handle: u64) -> i32 {
    boundary(Some(handle), || {
        with(handle, |entry| {
            if entry.response.is_some() {
                return Status::Busy as i32;
            }
            match entry.client.drain() {
                Ok(message) => retain(entry, &message),
                Err(e) => status(e),
            }
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_query(handle: u64) -> i32 {
    boundary(Some(handle), || {
        with(handle, |entry| {
            if entry.response.is_some() {
                return Status::Busy as i32;
            }
            let bytes = serde_json::to_vec(&entry.client.query());
            match bytes {
                Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => {
                    entry.response_after_stop = entry.client.terminal.is_some();
                    entry.response = Some(bytes);
                    Status::Ok as i32
                }
                _ => {
                    entry.client.stop();
                    Status::Limit as i32
                }
            }
        })
    })
}
/// # Safety
/// length_out must be aligned/writable usize and must not overlap SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_response_size(handle: u64, length_out: *mut usize) -> i32 {
    boundary(Some(handle), || {
        if length_out.is_null() || !length_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| match &entry.response {
            Some(bytes) => {
                unsafe {
                    length_out.write(bytes.len());
                }
                Status::Ok as i32
            }
            None => Status::NoResponse as i32,
        })
    })
}
/// # Safety
/// output is writable for capacity; NULL only with zero capacity. length_out is
/// aligned/writable usize; all allocations disjoint/live for the call. No NUL added.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_response_read(
    handle: u64,
    output: *mut u8,
    capacity: usize,
    length_out: *mut usize,
) -> i32 {
    boundary(Some(handle), || {
        if length_out.is_null() || !length_out.is_aligned() || (output.is_null() && capacity != 0) {
            return Status::InvalidArgument as i32;
        }
        with(handle, |entry| {
            let Some(bytes) = &entry.response else {
                return Status::NoResponse as i32;
            };
            unsafe {
                length_out.write(bytes.len());
            }
            if capacity < bytes.len() {
                return Status::BufferTooSmall as i32;
            }
            if !bytes.is_empty() {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
                }
            }
            entry.response = None;
            Status::Ok as i32
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_response_discard(handle: u64) -> i32 {
    boundary(Some(handle), || {
        with(handle, |entry| {
            entry.response = None;
            Status::Ok as i32
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_stop(handle: u64) -> i32 {
    // Stop remains available after panic poisoning. A retained pre-stop message
    // must not reappear through response_read after the fence.
    boundary(Some(handle), || {
        CLIENTS.with(|clients| {
            let Ok(mut clients) = clients.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            let Some(entry) = clients.get_mut(&handle) else {
                return Status::InvalidHandle as i32;
            };
            entry.response = None;
            entry.client.stop();
            Status::Ok as i32
        })
    })
}
/// Returns a caller-owned, SYNCHRONIZE-only Windows thread handle. Obtain it
/// before returning to a wait-only UI loop; CloseHandle it after final destroy.
/// # Safety
/// completion_out is aligned/writable usize, disjoint from SDK/caller buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_native_completion_handle(
    handle: u64,
    completion_out: *mut usize,
) -> i32 {
    use std::os::windows::io::IntoRawHandle;
    boundary(Some(handle), || {
        if completion_out.is_null() || !completion_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        // Available even for poisoned entries, so panic cannot strand cleanup.
        CLIENTS.with(|clients| {
            let Ok(mut clients) = clients.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            let Some(entry) = clients.get_mut(&handle) else {
                return Status::InvalidHandle as i32;
            };
            match entry.client.completion_handle() {
                Ok(completion) => {
                    unsafe {
                        completion_out.write(completion.into_raw_handle() as usize);
                    }
                    Status::Ok as i32
                }
                Err(error) => status(error),
            }
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn pim_app_native_destroy(handle: u64) -> i32 {
    boundary(Some(handle), || {
        CLIENTS.with(|clients| {
            let Ok(mut clients) = clients.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            let Some(entry) = clients.get_mut(&handle) else {
                return Status::InvalidHandle as i32;
            };
            entry.response = None;
            entry.client.stop();
            if !entry.client.complete_destroy() {
                return Status::Busy as i32;
            }
            clients.remove(&handle);
            Status::Ok as i32
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        sync::mpsc,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };

    struct ExitBarrier {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl Drop for ExitBarrier {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.release.recv();
        }
    }
    thread_local! { static EXIT_BARRIER: RefCell<Option<ExitBarrier>> = const { RefCell::new(None) }; }

    #[test]
    fn terminal_wake_consumed_before_thread_exit_still_has_owner_waitable_cleanup() {
        let (mut client, _, wakes) = super::super::tests::mock_client();
        let shared = Arc::clone(&client.shared);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        // Hold the real Windows thread in TLS teardown after network logical
        // completion. A pre-exit atomic/event would report completion too early.
        client.worker = Some(std::thread::spawn(move || {
            EXIT_BARRIER.with(|slot| {
                *slot.borrow_mut() = Some(ExitBarrier {
                    entered: entered_tx,
                    release: release_rx,
                })
            });
            shared.finish(StopReason::Transport);
        }));
        let handle = NEXT.fetch_add(1, Ordering::Relaxed);
        CLIENTS.with(|clients| {
            clients.borrow_mut().insert(
                handle,
                Entry {
                    client,
                    response: None,
                    response_after_stop: false,
                    poisoned: false,
                },
            )
        });
        let mut raw = 0;
        assert_eq!(
            unsafe { pim_app_native_completion_handle(handle, &mut raw) },
            Status::Ok as i32
        );
        // SAFETY: successful export transferred one owned duplicate to caller.
        let completion = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker in TLS epilogue");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        // Owner consumed the only terminal message. Its HWND may now be gone;
        // neither an arrival nor another HWND wake is needed for completion.
        CLIENTS.with(|clients| {
            let mut clients = clients.borrow_mut();
            let client = &mut clients.get_mut(&handle).unwrap().client;
            assert!(client.drain().unwrap().is_none());
            assert!(!client.query().status.local_worker_retired);
            assert!(client.worker.is_some());
        });
        assert_eq!(
            unsafe { WaitForSingleObject(completion.as_raw_handle() as HANDLE, 0) },
            WAIT_TIMEOUT
        );
        assert_eq!(pim_app_native_stop(handle), Status::Ok as i32);
        assert_eq!(pim_app_native_destroy(handle), Status::Busy as i32);
        release_tx.send(()).unwrap();
        // In the real owner loop this handle joins MsgWaitForMultipleObjectsEx,
        // including after WM_NCDESTROY; this test uses a bounded wait, no HWND.
        assert_eq!(
            unsafe { WaitForSingleObject(completion.as_raw_handle() as HANDLE, 2000) },
            WAIT_OBJECT_0
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(pim_app_native_destroy(handle), Status::Ok as i32);
        assert_eq!(pim_app_native_query(handle), Status::InvalidHandle as i32);
        // The independent duplicate remains valid until caller closes it.
        assert_eq!(
            unsafe { WaitForSingleObject(completion.as_raw_handle() as HANDLE, 0) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn successful_destroy_consumes_actual_completed_join_handle() {
        let (mut client, _, _) = super::super::tests::mock_client();
        client.worker = Some(std::thread::spawn(|| {}));
        let completion = client.completion_handle().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(completion.as_raw_handle() as HANDLE, 2000) },
            WAIT_OBJECT_0
        );
        assert!(client.worker.is_some());
        assert!(client.complete_destroy());
        assert!(client.worker.is_none());
    }

    #[test]
    #[ignore = "requires explicitly approved disposable Windows own-window runner"]
    fn stop_in_nc_destroy_then_owner_waits_without_hwnd_for_actual_completion() {
        use std::cell::Cell;
        use windows_sys::Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            UI::WindowsAndMessaging::*,
        };
        thread_local! {
            static WINDOW_CLIENT: Cell<u64> = const { Cell::new(0) };
            static WINDOW_STOPPED: Cell<bool> = const { Cell::new(false) };
            static PREVIOUS_PROC: Cell<WNDPROC> = const { Cell::new(None) };
        }
        unsafe extern "system" fn window_proc(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if message == WM_NCDESTROY {
                WINDOW_CLIENT.with(|token| {
                    WINDOW_STOPPED.with(|stopped| {
                        stopped.set(pim_app_native_stop(token.get()) == Status::Ok as i32)
                    })
                });
            }
            let previous = PREVIOUS_PROC.with(Cell::get);
            unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
        }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "PIM completion fixture\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let (mut client, _, _) = super::super::tests::mock_client();
        let binding = wake::Binding::new(hwnd as usize).unwrap();
        Arc::get_mut(&mut client.shared).unwrap().wake = binding.target.clone();
        client.binding = binding;
        let shared = Arc::clone(&client.shared);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        client.worker = Some(std::thread::spawn(move || {
            EXIT_BARRIER.with(|slot| {
                *slot.borrow_mut() = Some(ExitBarrier {
                    entered: entered_tx,
                    release: release_rx,
                })
            });
            shared.finish(StopReason::Transport);
        }));
        let wake_message = client.binding.target.message;
        let token = NEXT.fetch_add(1, Ordering::Relaxed);
        CLIENTS.with(|clients| {
            clients.borrow_mut().insert(
                token,
                Entry {
                    client,
                    response: None,
                    response_after_stop: false,
                    poisoned: false,
                },
            )
        });
        WINDOW_CLIENT.with(|slot| slot.set(token));
        let old = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, window_proc as *const () as _) };
        assert_ne!(old, 0);
        #[cfg(target_pointer_width = "64")]
        let previous_bits = old;
        #[cfg(target_pointer_width = "32")]
        let previous_bits = old as isize;
        // SAFETY: successful subclassing returned the prior WNDPROC of this HWND.
        PREVIOUS_PROC
            .with(|slot| slot.set(unsafe { std::mem::transmute::<isize, WNDPROC>(previous_bits) }));
        let mut raw = 0;
        assert_eq!(
            unsafe { pim_app_native_completion_handle(token, &mut raw) },
            Status::Ok as i32
        );
        let completion = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut message: MSG = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { PeekMessageW(&mut message, hwnd, wake_message, wake_message, PM_REMOVE) },
            0
        );
        assert_eq!(pim_app_native_drain(token), Status::Ok as i32);
        assert_eq!(pim_app_native_response_discard(token), Status::Ok as i32);
        assert_ne!(unsafe { DestroyWindow(hwnd) }, 0);
        assert!(WINDOW_STOPPED.with(Cell::get));
        assert_eq!(unsafe { IsWindow(hwnd) }, 0);
        assert_eq!(pim_app_native_destroy(token), Status::Busy as i32);
        release_tx.send(()).unwrap();
        let wait = completion.as_raw_handle() as HANDLE;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "bounded completion after HWND death");
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(
                    1,
                    &wait,
                    remaining.as_millis().min(2000) as u32,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if result == WAIT_OBJECT_0 {
                break;
            }
            assert_eq!(result, WAIT_OBJECT_0 + 1);
            while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0
            {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        assert_eq!(pim_app_native_destroy(token), Status::Ok as i32);
        WINDOW_CLIENT.with(|slot| slot.set(0));
        PREVIOUS_PROC.with(|slot| slot.set(None));
    }

    fn fixture() -> u64 {
        let (client, _rx, _) = super::super::tests::mock_client();
        let handle = NEXT.fetch_add(1, Ordering::Relaxed);
        CLIENTS.with(|clients| {
            clients.borrow_mut().insert(
                handle,
                Entry {
                    client,
                    response: Some(b"retained-once".to_vec()),
                    response_after_stop: false,
                    poisoned: false,
                },
            )
        });
        handle
    }
    #[test]
    fn short_buffers_retain_once_and_wrong_thread_tokens_are_invalid() {
        let handle = fixture();
        let mut size = 0;
        assert_eq!(
            unsafe { pim_app_native_response_read(handle, std::ptr::null_mut(), 0, &mut size) },
            Status::BufferTooSmall as i32
        );
        assert_eq!(size, 13);
        let mut bytes = vec![0; size];
        assert_eq!(
            unsafe {
                pim_app_native_response_read(handle, bytes.as_mut_ptr(), bytes.len(), &mut size)
            },
            Status::Ok as i32
        );
        assert_eq!(bytes, b"retained-once");
        assert_eq!(
            unsafe { pim_app_native_response_size(handle, &mut size) },
            Status::NoResponse as i32
        );
        assert_eq!(
            std::thread::spawn(move || pim_app_native_query(handle))
                .join()
                .unwrap(),
            Status::InvalidHandle as i32
        );
        assert_eq!(pim_app_native_destroy(handle), Status::Ok as i32);
    }
    #[test]
    fn asynchronous_stop_invalidates_retained_payload_but_keeps_status_query_readable() {
        let handle = fixture();
        CLIENTS.with(|clients| {
            clients.borrow()[&handle]
                .client
                .shared
                .finish(StopReason::Transport)
        });
        let mut size = 0;
        assert_eq!(
            unsafe { pim_app_native_response_size(handle, &mut size) },
            Status::NoResponse as i32
        );
        assert_eq!(pim_app_native_query(handle), Status::Ok as i32);
        assert_eq!(
            unsafe { pim_app_native_response_size(handle, &mut size) },
            Status::Ok as i32
        );
        let mut bytes = vec![0; size];
        assert_eq!(
            unsafe {
                pim_app_native_response_read(handle, bytes.as_mut_ptr(), bytes.len(), &mut size)
            },
            Status::Ok as i32
        );
        let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(state["status"]["remote_cleanup_unconfirmed"], true);
        assert_eq!(state["users"], serde_json::json!([]));
        assert_eq!(pim_app_native_destroy(handle), Status::Ok as i32);
    }
    #[test]
    fn null_and_oversize_requests_reject_before_read_or_admission() {
        let mut id = 0;
        assert_eq!(
            unsafe { pim_app_native_request(1, std::ptr::null(), 4, &mut id) },
            Status::InvalidArgument as i32
        );
        assert_eq!(
            unsafe {
                pim_app_native_connect(
                    0,
                    std::ptr::null(),
                    MAX_REQUEST_BYTES + 1,
                    &mut id,
                    &mut 0u32,
                )
            },
            Status::InvalidArgument as i32
        );
        assert_eq!(id, 0);
    }
}
