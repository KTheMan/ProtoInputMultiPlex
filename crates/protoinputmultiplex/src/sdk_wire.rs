//! Private, versioned framing for the optional loopback SDK listener.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

pub(crate) const VERSION: u8 = 1;
pub(crate) const HELLO_LIMIT: usize = 4 * 1024;
// profile_document accepts 16 MiB of UTF-8 plus JSON-RPC and SDK framing.
pub(crate) const MESSAGE_LIMIT: usize = 17 * 1024 * 1024;
// Structured rmcp results include a text copy as well as structuredContent.
pub(crate) const RESULT_LIMIT: usize = 128 * 1024 * 1024;
pub(crate) const MAX_CONNECTIONS: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Endpoint {
    pub protoinputmultiplex_sdk: u8,
    pub port: u16,
    pub token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protoinputmultiplex_sdk: u8,
    #[serde(rename = "type")]
    kind: String,
    token: String,
}

#[derive(Serialize)]
struct Ready {
    protoinputmultiplex_sdk: u8,
    #[serde(rename = "type")]
    kind: &'static str,
}

pub(crate) fn ready() -> Result<String> {
    Ok(serde_json::to_string(&Ready {
        protoinputmultiplex_sdk: VERSION,
        kind: "ready",
    })?)
}

pub(crate) fn validate_hello(frame: &[u8], expected_token: &str) -> Result<()> {
    ensure!(frame.len() <= HELLO_LIMIT, "SDK hello frame is too large");
    let hello: Hello = serde_json::from_slice(frame)?;
    ensure!(
        hello.protoinputmultiplex_sdk == VERSION,
        "Unsupported SDK version"
    );
    ensure!(hello.kind == "hello", "SDK hello frame is required");
    ensure!(
        valid_token(&hello.token) && constant_time_eq(&hello.token, expected_token),
        "SDK authentication failed"
    );
    Ok(())
}

pub(crate) fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Serialization is bounded before any partial JSON is handed to the socket.
pub(crate) fn encode_result(value: &impl Serialize) -> Result<String> {
    encode_with_limit(value, RESULT_LIMIT)
}

fn encode_with_limit(value: &impl Serialize, limit: usize) -> Result<String> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("SDK result exceeds the 128 MiB limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value)?;
    Ok(String::from_utf8(output.bytes)?)
}

fn constant_time_eq(presented: &str, expected: &str) -> bool {
    let a = presented.as_bytes();
    let b = expected.as_bytes();
    let mut differences = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        differences |=
            usize::from(a.get(index).copied().unwrap_or(0) ^ b.get(index).copied().unwrap_or(0));
    }
    differences == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_requires_exact_version_kind_and_bearer() {
        let token = "a".repeat(64);
        let valid = serde_json::json!({"protoinputmultiplex_sdk":1,"type":"hello","token":token});
        validate_hello(valid.to_string().as_bytes(), &token).unwrap();
        for bad in [
            serde_json::json!({"protoinputmultiplex_sdk":2,"type":"hello","token":token}),
            serde_json::json!({"protoinputmultiplex_sdk":1,"type":"command","token":token}),
            serde_json::json!({"protoinputmultiplex_sdk":1,"type":"hello","token":"b".repeat(64)}),
            serde_json::json!({"protoinputmultiplex_sdk":1,"type":"hello","token":token,"grant":true}),
        ] {
            assert!(validate_hello(bad.to_string().as_bytes(), &token).is_err());
        }
        assert!(validate_hello(&vec![b'x'; HELLO_LIMIT + 1], &token).is_err());
    }

    #[test]
    fn output_serialization_counts_json_escapes_and_rejects_before_publish() {
        let escaped = "\"\\\n";
        let encoded = serde_json::to_string(escaped).unwrap();
        assert_eq!(encode_with_limit(&escaped, encoded.len()).unwrap(), encoded);
        assert!(encode_with_limit(&escaped, encoded.len() - 1).is_err());
    }
}
