//! Shared Rust ABI declaration metadata. Build.rs generates the C header from it;
//! ABI type-assignment tests check the Rust exports against these declarations.
use std::fmt::Write;

pub const ABI_VERSION: u32 = 1;
pub const MAX_HOSTS_PER_THREAD: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    Ok = 0,
    InvalidArgument = 1,
    InvalidHandle = 2,
    Busy = 3,
    BufferTooSmall = 4,
    NoResponse = 5,
    Limit = 6,
    Panic = 7,
    Poisoned = 8,
}

#[derive(Debug, Clone, Copy)]
pub enum CType {
    I32,
    U32,
    U64,
    Size,
    ConstBytes,
    MutBytes,
    MutU64,
    MutU32,
    MutSize,
}
impl CType {
    pub fn spelling(self) -> &'static str {
        match self {
            Self::I32 => "int32_t",
            Self::U32 => "uint32_t",
            Self::U64 => "uint64_t",
            Self::Size => "size_t",
            Self::ConstBytes => "const uint8_t *",
            Self::MutBytes => "uint8_t *",
            Self::MutU64 => "uint64_t *",
            Self::MutU32 => "uint32_t *",
            Self::MutSize => "size_t *",
        }
    }
}

pub struct Parameter {
    pub name: &'static str,
    pub kind: CType,
}
pub struct Function {
    pub name: &'static str,
    pub result: CType,
    pub parameters: &'static [Parameter],
}
use CType::*;

pub const FUNCTIONS: &[Function] = &[
    Function {
        name: "pim_app_abi_version",
        result: U32,
        parameters: &[],
    },
    Function {
        name: "pim_app_host_create",
        result: I32,
        parameters: &[
            Parameter {
                name: "epoch",
                kind: U64,
            },
            Parameter {
                name: "screens_json",
                kind: ConstBytes,
            },
            Parameter {
                name: "length",
                kind: Size,
            },
            Parameter {
                name: "host_out",
                kind: MutU64,
            },
        ],
    },
    Function {
        name: "pim_app_host_destroy",
        result: I32,
        parameters: &[Parameter {
            name: "host",
            kind: U64,
        }],
    },
    Function {
        name: "pim_app_host_call",
        result: I32,
        parameters: &[
            Parameter {
                name: "host",
                kind: U64,
            },
            Parameter {
                name: "request_json",
                kind: ConstBytes,
            },
            Parameter {
                name: "length",
                kind: Size,
            },
        ],
    },
    Function {
        name: "pim_app_client_call",
        result: I32,
        parameters: &[
            Parameter {
                name: "host",
                kind: U64,
            },
            Parameter {
                name: "bound_recipient",
                kind: U64,
            },
            Parameter {
                name: "request_json",
                kind: ConstBytes,
            },
            Parameter {
                name: "length",
                kind: Size,
            },
        ],
    },
    Function {
        name: "pim_app_response_size",
        result: I32,
        parameters: &[
            Parameter {
                name: "host",
                kind: U64,
            },
            Parameter {
                name: "length_out",
                kind: MutSize,
            },
        ],
    },
    Function {
        name: "pim_app_response_read",
        result: I32,
        parameters: &[
            Parameter {
                name: "host",
                kind: U64,
            },
            Parameter {
                name: "output",
                kind: MutBytes,
            },
            Parameter {
                name: "capacity",
                kind: Size,
            },
            Parameter {
                name: "length_out",
                kind: MutSize,
            },
        ],
    },
    Function {
        name: "pim_app_response_discard",
        result: I32,
        parameters: &[Parameter {
            name: "host",
            kind: U64,
        }],
    },
];

pub fn render_header() -> String { render_header_for(false) }
pub fn render_header_for(native_client: bool) -> String {
    let mut header = String::from(
        "/* Generated from mux-application-sdk Rust ABI metadata. Do not edit. */\n#ifndef PROTOINPUTMULTIPLEX_APPLICATION_SDK_H\n#define PROTOINPUTMULTIPLEX_APPLICATION_SDK_H\n#include <stddef.h>\n#include <stdint.h>\n#ifdef __cplusplus\nextern \"C\" {\n#endif\n#ifndef PIM_APP_API\n#define PIM_APP_API\n#endif\n#ifdef _WIN32\n#define PIM_APP_CALL __cdecl\n#else\n#define PIM_APP_CALL\n#endif\n",
    );
    writeln!(header, "#define PIM_APP_ABI_VERSION {ABI_VERSION}u").unwrap();
    writeln!(
        header,
        "#define PIM_APP_MAX_HOSTS_PER_THREAD {MAX_HOSTS_PER_THREAD}u"
    )
    .unwrap();
    for (name, status) in [
        ("OK", Status::Ok),
        ("INVALID_ARGUMENT", Status::InvalidArgument),
        ("INVALID_HANDLE", Status::InvalidHandle),
        ("BUSY", Status::Busy),
        ("BUFFER_TOO_SMALL", Status::BufferTooSmall),
        ("NO_RESPONSE", Status::NoResponse),
        ("LIMIT", Status::Limit),
        ("PANIC", Status::Panic),
        ("POISONED", Status::Poisoned),
    ] {
        writeln!(header, "#define PIM_APP_{name} {}", status as i32).unwrap();
    }
    header.push_str("/* Every host is confined to its creating thread; a wrong-thread token is invalid.\n * Host calls are trusted local setup, not an authentication boundary.\n * Client calls take a transport-bound recipient outside the JSON request.\n * Requests are UTF-8 JSON, at most 65536 bytes, with no trailing NUL.\n * Each successful call queues one JSON reply. Read/discard it before another call.\n * response_size does not consume. A short response_read returns BUFFER_TOO_SMALL,\n * writes the required byte count, and retains the same reply; never re-execute.\n * Output has no trailing NUL. On OK, response_read copies then consumes the reply.\n * All pointers must be valid for the stated length, scalar outputs aligned, and\n * input/output/scalar buffers must not overlap. NULL is allowed only for a zero\n * capacity response output. Invalid non-NULL memory cannot be validated by the SDK.\n * Caught Rust panics retire and poison the affected host; destroy it on its owner\n * thread. Panic=abort, OOM, process termination, and caller memory violations are\n * outside catch_unwind. No native event callback crosses this ABI.\n */\n");
    for function in FUNCTIONS.iter().chain(if native_client { NATIVE_FUNCTIONS.iter() } else { [].iter() }) {
        write!(
            header,
            "PIM_APP_API {} PIM_APP_CALL {}(",
            function.result.spelling(),
            function.name
        )
        .unwrap();
        if function.parameters.is_empty() {
            header.push_str("void");
        }
        for (index, parameter) in function.parameters.iter().enumerate() {
            if index != 0 {
                header.push_str(", ");
            }
            write!(header, "{} {}", parameter.kind.spelling(), parameter.name).unwrap();
        }
        header.push_str(");\n");
    }
    if native_client { header.push_str(NATIVE_CONTRACT); }
    header.push_str("#ifdef __cplusplus\n}\n#endif\n#endif\n");
    header
}

pub const NATIVE_FUNCTIONS: &[Function] = &[
    Function { name: "pim_app_native_connect", result: I32, parameters: &[
        Parameter { name: "own_hwnd", kind: Size }, Parameter { name: "config_json", kind: ConstBytes }, Parameter { name: "length", kind: Size }, Parameter { name: "client_out", kind: MutU64 }, Parameter { name: "wake_message_out", kind: MutU32 }] },
    Function { name: "pim_app_native_enable_input", result: I32, parameters: &[
        Parameter { name: "client", kind: U64 }, Parameter { name: "pointer_enabled", kind: U32 }, Parameter { name: "keys_enabled", kind: U32 }] },
    Function { name: "pim_app_native_request", result: I32, parameters: &[
        Parameter { name: "client", kind: U64 }, Parameter { name: "request_json", kind: ConstBytes }, Parameter { name: "length", kind: Size }, Parameter { name: "request_id_out", kind: MutU64 }] },
    Function { name: "pim_app_native_drain", result: I32, parameters: &[Parameter { name: "client", kind: U64 }] },
    Function { name: "pim_app_native_query", result: I32, parameters: &[Parameter { name: "client", kind: U64 }] },
    Function { name: "pim_app_native_response_size", result: I32, parameters: &[
        Parameter { name: "client", kind: U64 }, Parameter { name: "length_out", kind: MutSize }] },
    Function { name: "pim_app_native_response_read", result: I32, parameters: &[
        Parameter { name: "client", kind: U64 }, Parameter { name: "output", kind: MutBytes }, Parameter { name: "capacity", kind: Size }, Parameter { name: "length_out", kind: MutSize }] },
    Function { name: "pim_app_native_response_discard", result: I32, parameters: &[Parameter { name: "client", kind: U64 }] },
    Function { name: "pim_app_native_stop", result: I32, parameters: &[Parameter { name: "client", kind: U64 }] },
    Function { name: "pim_app_native_completion_handle", result: I32, parameters: &[
        Parameter { name: "client", kind: U64 }, Parameter { name: "completion_out", kind: MutSize }] },
    Function { name: "pim_app_native_destroy", result: I32, parameters: &[Parameter { name: "client", kind: U64 }] },
];
const NATIVE_CONTRACT: &str = "\n#define PIM_APP_NATIVE_CLIENT_ABI_VERSION 2u\n/* Native client exports exist only in Windows builds with native-client enabled.\n * This external client connects to the GUI's approved event server; it creates no\n * Host or users. Explicit connect plus local pointer/key enables default off.\n * config_json: {endpoint,exact_origin,event_credential}; transient memory only.\n * Endpoint is ws://127.0.0.1:PORT/application-events/v1; no credential in URL/path.\n * Own top-level HWND must belong to this process and creating UI thread.\n * connect returns handle and wake_message. Before yielding to the UI loop, call\n * completion_handle and retain its caller-owned SYNCHRONIZE-only HANDLE. Its\n * OS thread signal survives HWND destruction; CloseHandle it after destroy OK.\n * If duplication fails, retain the client/DLL and retry cleanup; never abandon\n * an active token then unload. completion_handle leaves its output unchanged on failure.\n * query returns wake_message, Ready permissions, validated model and retirement.\n * Handle tokens are owner-thread only, max16 clients/workers. Host/client token\n * families are disjoint API namespaces; never interchange their numeric values.\n * WndProc recognizes the registered wake_message (wParam=lParam=0), then calls\n * drain and reads each retained UTF-8 JSON ServerMessage until JSON null.\n * The message is a coalesced hint: it contains no payload or object pointer.\n * Drain installs snapshots/folds streams before delivery. Query reads model state.\n * After explicit Gap, request Resubscribe with the current subscription token;\n * no automatic reconnect, retry, resubscribe, or uncertain command replay.\n * request OK returns local request_id admission only. Matched Response is remote\n * result. Pending requests become uncertain on timeout/EOF/Stop, even if unsent.\n * enable_input accepts only0/1. Disabling a previously enabled family stops this\n * connection to reconcile producer holds through server EOF cleanup. Reconnect\n * requires fresh explicit setup/credential approval, never replays old commands.\n * Native response size/read/discard has the Host retained-buffer contract.\n * Short reads preserve bytes; do not call request again to resize output.\n * Stop clears local model, queued/retained payloads, and cancels socket I/O.\n * Stop on owner thread in WM_NCDESTROY before HWND reuse, then return promptly.\n * Keep the owner message loop alive after its last HWND: include completion HANDLEs\n * in MsgWaitForMultipleObjectsEx with MWMO_INPUTAVAILABLE and drain normal messages.\n * Handle signaled completion by calling destroy on the owner thread. Remove and\n * CloseHandle that wait handle only after destroy OK; no event timer is required.\n * local_worker_retired is actual OS termination, not logical network completion.\n * destroy returns BUSY until a zero-time OS termination probe and is_finished\n * both succeed; OK consumes the completed Rust JoinHandle before returning.\n * No blocking network join, reaper, completion callback, or pre-exit signal is used.\n * Before owner thread exit, stop and successfully destroy EVERY owned client.\n * Native registry TLS has no destructor: explicit destroy removes each entry.\n * Owner exit with active tokens leaks state and forbids unload for process lifetime.\n * Rust Drop emergency detach or owner exit never grants DLL unload safety.\n * Keep the DLL loaded until all owners have explicitly destroyed every client,\n * all SDK calls/frames have returned, and no later calls or callbacks can enter.\n * Do not invoke cleanup or waits under DllMain/loader lock. A caller that abandons\n * a client must keep the DLL loaded for the remaining process lifetime.\n * remote_cleanup_unconfirmed stays true: TCP EOF/local retirement does not prove\n * GUI HostBridge cleanup. Verify authoritative host holds separately.\n * No native callbacks, global input, hooks, clipboard, cursor clipping or injection.\n */\n";
