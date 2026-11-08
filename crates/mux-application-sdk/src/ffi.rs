//! Stable local C embedding ABI. JSON envelopes share the typed Rust API, not MCP.
//! One bounded pending response separates execution from buffer copying so small
//! C output buffers cannot accidentally replay a mutating command.
use crate::abi_contract::{ABI_VERSION, MAX_HOSTS_PER_THREAD, Status};
use crate::{Host, PlatformWindows, protocol::*};
use mux_core::application_event::{RecipientId, ScreenState};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    mem::ManuallyDrop,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU64, Ordering},
};

pub const C_HEADER: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/protoinputmultiplex_application_sdk.h"
));
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

struct Entry {
    host: Host<PlatformWindows>,
    response: Option<Vec<u8>>,
    poisoned: bool,
}
struct Registry {
    hosts: BTreeMap<u64, Entry>,
}
// Explicit owner-thread destruction is required before thread exit or DLL unload.
// Never run Host/WindowAdapter cleanup from an implicit DLL TLS detach callback.
// An orphaned entry intentionally leaks; it cannot be recovered by another thread
// and forfeits unload permission for this loaded SDK instance's process lifetime.
type HostRegistry = RefCell<ManuallyDrop<Registry>>;
const _: () = assert!(!std::mem::needs_drop::<HostRegistry>());
thread_local! { static REGISTRY: HostRegistry = const { RefCell::new(ManuallyDrop::new(Registry { hosts: BTreeMap::new() })) }; }

fn boundary(handle: Option<u64>, operation: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(status) => status,
        Err(_) => {
            if let Some(handle) = handle {
                // Unwinding has released RefCell borrows. Poison before attempting
                // cleanup, and contain any secondary backend cleanup panic as well.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    REGISTRY.with(|registry| {
                        if let Ok(mut registry) = registry.try_borrow_mut()
                            && let Some(entry) = registry.hosts.get_mut(&handle)
                        {
                            entry.poisoned = true;
                            entry.response = None;
                            entry.host.end();
                        }
                    })
                }));
            }
            Status::Panic as i32
        }
    }
}

fn with_entry(handle: u64, operation: impl FnOnce(&mut Entry) -> i32) -> i32 {
    REGISTRY.with(|registry| {
        let Ok(mut registry) = registry.try_borrow_mut() else {
            return Status::Busy as i32;
        };
        let Some(entry) = registry.hosts.get_mut(&handle) else {
            return Status::InvalidHandle as i32;
        };
        if entry.poisoned {
            return Status::Poisoned as i32;
        }
        operation(entry)
    })
}

unsafe fn decode<T: DeserializeOwned>(input: *const u8, length: usize) -> Result<T, Status> {
    if input.is_null() || length == 0 || length > MAX_REQUEST_BYTES {
        return Err(Status::InvalidArgument);
    }
    // SAFETY: the exported function's C caller guarantees the allocation is live
    // and readable for length bytes. Length is bounded before constructing a slice.
    let bytes = unsafe { std::slice::from_raw_parts(input, length) };
    let text = std::str::from_utf8(bytes).map_err(|_| Status::InvalidArgument)?;
    serde_json::from_str(text).map_err(|_| Status::InvalidArgument)
}

fn queue<T: Serialize>(entry: &mut Entry, response: Reply<T>) -> i32 {
    match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => {
            entry.response = Some(bytes);
            Status::Ok as i32
        }
        _ => {
            entry.poisoned = true;
            entry.response = None;
            entry.host.end();
            Status::Limit as i32
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn pim_app_abi_version() -> u32 {
    ABI_VERSION
}

/// Creates one cooperative engine without creating recipients/users or enabling export.
/// # Safety
/// Non-NULL screens_json with length 1..=MAX_REQUEST_BYTES must be readable
/// for that many bytes. NULL/zero/oversize requests are rejected before reading.
/// A non-NULL host_out must point to an
/// aligned writable u64; buffers must not overlap. No pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_host_create(
    epoch: u64,
    screens_json: *const u8,
    length: usize,
    host_out: *mut u64,
) -> i32 {
    boundary(None, || {
        if host_out.is_null() || !host_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        let screens: ScreenState = match unsafe { decode(screens_json, length) } {
            Ok(value) => value,
            Err(status) => return status as i32,
        };
        let host = match Host::<PlatformWindows>::new(epoch, screens) {
            Ok(host) => host,
            Err(_) => return Status::InvalidArgument as i32,
        };
        REGISTRY.with(|registry| {
            let Ok(mut registry) = registry.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            if registry.hosts.len() >= MAX_HOSTS_PER_THREAD {
                return Status::Limit as i32;
            }
            let handle =
                match NEXT_HANDLE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                }) {
                    Ok(handle) => handle,
                    Err(_) => return Status::Limit as i32,
                };
            registry.hosts.insert(
                handle,
                Entry {
                    host,
                    response: None,
                    poisoned: false,
                },
            );
            // SAFETY: validated scalar pointer, caller supplies its live allocation.
            unsafe {
                host_out.write(handle);
            }
            Status::Ok as i32
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn pim_app_host_destroy(host: u64) -> i32 {
    boundary(Some(host), || {
        REGISTRY.with(|registry| {
            let Ok(mut registry) = registry.try_borrow_mut() else {
                return Status::Busy as i32;
            };
            // Removal synchronously drops Host: Engine::end, then native bindings.
            if registry.hosts.remove(&host).is_some() {
                Status::Ok as i32
            } else {
                Status::InvalidHandle as i32
            }
        })
    })
}

/// Executes a trusted host-only configuration request. Never expose this function
/// as a remote/client method; local host configuration is outside client authority.
/// # Safety
/// A non-NULL request_json with length 1..=MAX_REQUEST_BYTES must be readable
/// for that many bytes. NULL/zero/oversize requests are rejected before reading.
/// Request memory must not overlap output
/// buffers. Its allocation need only remain live until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_host_call(
    host: u64,
    request_json: *const u8,
    length: usize,
) -> i32 {
    boundary(Some(host), || {
        let request: HostRequest = match unsafe { decode(request_json, length) } {
            Ok(value) => value,
            Err(status) => return status as i32,
        };
        with_entry(host, |entry| {
            if entry.response.is_some() {
                return Status::Busy as i32;
            }
            let result = entry.host.host_dispatch(request);
            queue(entry, result.into())
        })
    })
}

/// Executes a client operation for an identity already bound by the trusted host
/// or authenticated transport. Recipient numbers alone are not credentials.
/// # Safety
/// A non-NULL request_json with length 1..=MAX_REQUEST_BYTES must be readable
/// for that many bytes. NULL/zero/oversize requests are rejected before reading.
/// Request memory must not overlap output
/// buffers. bound_recipient must come from trusted binding, not the JSON caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_client_call(
    host: u64,
    bound_recipient: u64,
    request_json: *const u8,
    length: usize,
) -> i32 {
    boundary(Some(host), || {
        let request: ClientRequest = match unsafe { decode(request_json, length) } {
            Ok(value) => value,
            Err(status) => return status as i32,
        };
        let recipient: RecipientId =
            match serde_json::from_value(serde_json::Value::from(bound_recipient)) {
                Ok(value) => value,
                Err(_) => return Status::InvalidArgument as i32,
            };
        with_entry(host, |entry| {
            if entry.response.is_some() {
                return Status::Busy as i32;
            }
            let result = entry.host.dispatch(recipient, request);
            queue(entry, result.into())
        })
    })
}

/// Queries the exact pending UTF-8 byte count without consuming it.
/// # Safety
/// length_out must point to an aligned writable usize for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_response_size(host: u64, length_out: *mut usize) -> i32 {
    boundary(Some(host), || {
        if length_out.is_null() || !length_out.is_aligned() {
            return Status::InvalidArgument as i32;
        }
        with_entry(host, |entry| {
            let Some(response) = &entry.response else {
                return Status::NoResponse as i32;
            };
            unsafe {
                length_out.write(response.len());
            }
            Status::Ok as i32
        })
    })
}

/// Copies and consumes a pending reply only on OK; short output retains the reply.
/// # Safety
/// output must be writable for capacity bytes (NULL allowed when capacity is zero).
/// length_out must be aligned/writable. These allocations must not overlap each
/// other or SDK-owned memory. Output is UTF-8 bytes without a trailing NUL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pim_app_response_read(
    host: u64,
    output: *mut u8,
    capacity: usize,
    length_out: *mut usize,
) -> i32 {
    boundary(Some(host), || {
        if length_out.is_null() || !length_out.is_aligned() || (capacity != 0 && output.is_null()) {
            return Status::InvalidArgument as i32;
        }
        with_entry(host, |entry| {
            let Some(response) = &entry.response else {
                return Status::NoResponse as i32;
            };
            unsafe {
                length_out.write(response.len());
            }
            if capacity < response.len() {
                return Status::BufferTooSmall as i32;
            }
            // response is nonempty JSON, so an adequate output cannot be NULL.
            if output.is_null() {
                return Status::InvalidArgument as i32;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(response.as_ptr(), output, response.len());
            }
            entry.response = None;
            Status::Ok as i32
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn pim_app_response_discard(host: u64) -> i32 {
    boundary(Some(host), || {
        with_entry(host, |entry| {
            if entry.response.take().is_some() {
                Status::Ok as i32
            } else {
                Status::NoResponse as i32
            }
        })
    })
}

#[cfg(test)]
pub(crate) fn panic_for_test(handle: u64) -> i32 {
    boundary(Some(handle), || {
        with_entry(handle, |_entry| panic!("intentional ABI boundary test"))
    })
}
