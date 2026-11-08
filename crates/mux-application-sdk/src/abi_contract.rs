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

pub fn render_header() -> String {
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
    for function in FUNCTIONS {
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
    header.push_str("#ifdef __cplusplus\n}\n#endif\n#endif\n");
    header
}
