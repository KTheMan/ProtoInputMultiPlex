//! Opt-in real server check. Never loads a backend, routes input or opens apps.
use std::{path::PathBuf, process::Command};

#[test]
#[ignore = "requires PIM_TEST_SERVER pointing to an approved built server; no routing"]
fn real_server_discovery_and_documents_without_native_dlls() {
    let server = PathBuf::from(std::env::var_os("PIM_TEST_SERVER").expect("set PIM_TEST_SERVER"));
    assert!(server.is_absolute() && server.is_file());
    let root = std::env::temp_dir().join(format!("pim-client-nonrouting-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = root.join("missing-backend");
    assert!(!runtime.exists(), "the DLL-free runtime must not exist");
    let preferences = root.join("disposable-preferences.json");
    assert!(!preferences.exists(), "use a fresh disposable document");
    for action in ["inspect", "check"] {
        let output = Command::new(env!("CARGO_BIN_EXE_protoinputmux-mcp-example"))
            .arg(action)
            .env("PIM_SERVER", &server)
            .env("PIM_RUNTIME", &runtime)
            .env("PIM_PREFERENCES", &preferences)
            .env_remove("PIM_GUI_CONNECTION")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "non-routing client command failed (raw output withheld)"
        );
        let summary = String::from_utf8(output.stdout).unwrap();
        assert!(summary.contains("Discovered ") && summary.contains("Workflow passed"));
        assert!(output.stderr.is_empty());
        assert!(
            !preferences.exists(),
            "denied preferences must not create a document"
        );
        assert!(
            !runtime.exists(),
            "discovery/documents must not create a runtime"
        );
    }
    // Keep only the empty test directory; no raw server response or credentials.
}
