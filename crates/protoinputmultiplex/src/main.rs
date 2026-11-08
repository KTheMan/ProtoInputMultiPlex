#[cfg(windows)]
mod application_event_transport;
#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use mux_core::Profile;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod app_paths;
#[cfg(windows)]
mod config_maintenance;
#[cfg(windows)]
mod cursor_settings_ui;
#[cfg(windows)]
mod installation;
mod profile_store;

#[cfg(windows)]
mod agent_export;
#[cfg(windows)]
mod agent_runtime;
#[cfg(windows)]
mod agent_ui;
#[cfg(windows)]
mod annotation_runtime;
#[cfg(windows)]
mod annotation_ui;
#[cfg(windows)]
mod app_lifecycle;
#[cfg(windows)]
mod app_preferences;
#[cfg(windows)]
mod app_preferences_ui;
#[cfg(windows)]
mod application_event_ui;
#[cfg(windows)]
mod automation_probe;
#[cfg(windows)]
mod automation_runtime;
#[cfg(windows)]
mod desktop_capture_fixture;
#[cfg(windows)]
mod desktop_router;
#[cfg(windows)]
mod emergency_stop;
mod forwarding_ui;
#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod gui_activity;
#[cfg(windows)]
mod gui_agent_broker;
#[cfg(windows)]
mod gui_app_preferences;
#[cfg(windows)]
mod gui_application_events;
#[cfg(windows)]
mod gui_bridge;
#[cfg(windows)]
mod gui_capture;
#[cfg(windows)]
mod gui_discovery;
#[cfg(windows)]
mod gui_macro_file;
#[cfg(windows)]
mod gui_profile_file;
#[cfg(windows)]
mod gui_profile_policy;
#[cfg(windows)]
mod gui_script_io;
#[cfg(windows)]
mod gui_sdk;
#[cfg(windows)]
mod gui_setup;
#[cfg(windows)]
mod gui_window_lock;
#[cfg(windows)]
mod keyboard_slider;
#[cfg(windows)]
mod macro_studio;
#[cfg(windows)]
mod macro_studio_ui;
#[cfg(windows)]
mod magnifier_runtime;
#[cfg(windows)]
mod magnifier_ui;
#[cfg(windows)]
mod mapper_flow_store;
#[cfg(windows)]
mod mapper_runtime;
#[cfg(windows)]
mod mapper_screen;
#[cfg(windows)]
mod mapper_ui;
#[cfg(windows)]
mod mcp;
#[cfg(windows)]
mod output_runtime;
#[cfg(windows)]
mod output_ui;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
mod replication_queue;
#[cfg(windows)]
mod replication_ui;
#[cfg(windows)]
mod router;
#[cfg(windows)]
mod routing_engine;
#[cfg(windows)]
mod screen_analysis;
#[cfg(windows)]
mod screen_ocr;
#[cfg(windows)]
mod screen_ui;
#[cfg(windows)]
mod sdk_listener;
#[cfg(windows)]
mod sdk_wire;
mod session;
#[cfg(windows)]
mod session_recovery;
#[cfg(windows)]
mod ui_script;
#[cfg(windows)]
mod virtual_users;
#[cfg(windows)]
mod virtual_users_ui;
#[cfg(windows)]
mod window_selection;

#[cfg(windows)]
const APP_NAME: &str = "ProtoInputMultiPlex";

/// Native GUI renderer; software mode never falls back to a hardware adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum GuiRenderer {
    #[default]
    Glow,
    SoftwareDx12,
}

#[derive(Parser)]
#[command(
    name = "protoinputmultiplex",
    version,
    about = "ProtoInputMultiPlex — independent input seats powered by Rust and ProtoInput"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Install, upgrade, recover, roll back or uninstall an owned distribution.
    #[cfg(windows)]
    Installation {
        #[command(subcommand)]
        command: installation::Command,
    },
    /// Inspect, migrate, back up or restore a chosen user-data directory.
    #[cfg(windows)]
    Config {
        #[command(subcommand)]
        command: config_maintenance::Command,
    },
    /// Serve virtual desktop routing tools over MCP stdio (no physical input).
    Mcp {
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Connect to an enabled GUI-owned session using its private descriptor file.
        #[arg(long)]
        gui_connection: Option<PathBuf>,
        /// Application-preferences document (separate from seat profiles).
        #[arg(long)]
        preferences: Option<PathBuf>,
        /// Explicitly allow standalone MCP access to application preferences.
        #[arg(long)]
        allow_app_preferences: bool,
        /// Allow standalone MCP to read and save named Mapper flows in its chosen data directory.
        #[arg(long, requires = "data_dir", conflicts_with = "gui_connection")]
        allow_mapper_library: bool,
        /// Root for preferences, profiles, macros and Mapper flows; defaults to LocalAppData/ProtoInputMultiPlex.
        #[arg(long, conflicts_with = "preferences")]
        data_dir: Option<PathBuf>,
    },
    /// Open the device dashboard and live multi-mouse preview (default).
    Gui {
        /// Native GUI renderer (software-dx12 requires the software-renderer build feature).
        #[arg(long, value_enum, default_value_t = GuiRenderer::Glow)]
        renderer: GuiRenderer,
        /// Root for preferences, profiles and macros; defaults to LocalAppData/ProtoInputMultiPlex.
        #[arg(long, conflicts_with = "preferences")]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        profile: Option<PathBuf>,
        #[arg(long, hide = true)]
        ui_script: Option<PathBuf>,
        /// Skip renderer screenshots while retaining app-local script diagnostics.
        #[arg(long, hide = true, requires = "ui_script")]
        ui_script_no_screenshots: bool,
        /// Opt-in bounded discovery delay for owned GUI responsiveness acceptance.
        #[arg(long, hide = true, requires = "ui_script", value_parser = clap::value_parser!(u64).range(1..=15_000))]
        discovery_delay_ms: Option<u64>,
        /// Opt-in bounded Start preflight delay for owned GUI responsiveness acceptance.
        #[arg(long, hide = true, requires = "ui_script", value_parser = clap::value_parser!(u64).range(1..=15_000))]
        start_preflight_delay_ms: Option<u64>,
        /// Artificial worker-only delay for opt-in profile file responsiveness acceptance.
        #[arg(long, hide = true, requires = "ui_script", value_parser = clap::value_parser!(u64).range(1..=15_000))]
        profile_file_delay_ms: Option<u64>,
        /// Artificial worker-only delay for opt-in application preference acceptance.
        #[arg(long, hide = true, requires = "ui_script", value_parser = clap::value_parser!(u64).range(1..=15_000))]
        app_preferences_delay_ms: Option<u64>,
        /// One worker-only delay after script handoff for bootstrap responsiveness checks.
        #[arg(long, hide = true, requires = "ui_script", value_parser = clap::value_parser!(u64).range(1..=15_000))]
        bootstrap_delay_ms: Option<u64>,
        /// Application-preferences document (separate from seat profiles).
        #[arg(long)]
        preferences: Option<PathBuf>,
        /// Test-only current-user registry path under Software\\ProtoInputMultiPlex\\Tests\\.
        #[arg(long, hide = true)]
        startup_test_key: Option<String>,
    },
    /// Open a disposable target window with input counters for routing validation.
    Probe {
        #[arg(long)]
        report: Option<PathBuf>,
        /// Opt-in native text/key trace fixture for synthetic automation acceptance.
        #[arg(long)]
        automation: bool,
        /// Opt-in owned capture fixture RGB color (decimal 0..16777215).
        #[arg(long, hide = true)]
        capture_pattern: Option<u32>,
        /// Opt-in app cursor regions for disposable virtual routing acceptance.
        #[arg(long, hide = true)]
        cursor_fixture: bool,
        /// Opt-in real focused/caret native control fixture for virtual acceptance.
        #[arg(long, hide = true)]
        keyboard_focus_fixture: bool,
        /// Opt-in ordinary EDIT click/Tab fixture without cooperative focus handling.
        #[arg(
            long,
            hide = true,
            requires = "automation",
            conflicts_with_all = ["capture_pattern", "cursor_fixture", "keyboard_focus_fixture"]
        )]
        native_controls_fixture: bool,
        /// Opt-in real Ctrl+S accelerator fixture, layered on ordinary native controls.
        #[arg(long, hide = true, requires = "native_controls_fixture")]
        accelerator_fixture: bool,
        /// Opt-in repeated PM_NOREMOVE and synchronous keyboard-state observations.
        #[arg(long, hide = true, requires = "accelerator_fixture")]
        keyboard_state_fixture: bool,
    },
    /// Opt-in disposable opaque desktop fixture for bounded screenshot-export validation.
    #[cfg(windows)]
    #[command(hide = true)]
    DesktopCaptureFixture {
        #[arg(long)]
        report: PathBuf,
        #[arg(
            long,
            default_value_t = 180,
            value_parser = clap::value_parser!(u64).range(1..=180)
        )]
        max_seconds: u64,
    },
    /// Internal shared emergency-stop owner for this interactive desktop.
    #[command(hide = true)]
    EmergencyBroker,
    /// Internal child process, owned by the control center.
    #[command(hide = true)]
    SessionWorker {
        #[arg(long)]
        runtime: PathBuf,
        #[arg(long, default_value_t = 0)]
        controller_pid: u32,
    },
    /// Print connected Raw Input mice and keyboards as JSON.
    Devices,
    /// Validate a profile, optionally requiring its devices to be connected.
    Validate {
        profile: PathBuf,
        #[arg(long)]
        connected: bool,
    },
    /// Load the backend DLL and verify every required API symbol, without injection.
    Doctor {
        #[arg(long)]
        runtime: Option<PathBuf>,
    },
    /// Observe mouse packets for a bounded interval; does not inject or block input.
    Observe {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Attach a profile's devices to its explicit target PIDs until Enter is pressed.
    Attach {
        profile: PathBuf,
        #[arg(long)]
        runtime: Option<PathBuf>,
    },
}

pub(crate) fn read_profile(path: &Path) -> Result<Profile> {
    profile_store::read(path)
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Command::Validate {
        ref profile,
        connected: false,
    }) = args.command
    {
        let profile = read_profile(profile)?;
        println!("Valid profile: {} seat(s)", profile.seats.len());
        return Ok(());
    }
    #[cfg(windows)]
    {
        windows_main(args.command.unwrap_or(Command::Gui {
            renderer: GuiRenderer::Glow,
            data_dir: None,
            profile: None,
            ui_script: None,
            ui_script_no_screenshots: false,
            discovery_delay_ms: None,
            start_preflight_delay_ms: None,
            profile_file_delay_ms: None,
            app_preferences_delay_ms: None,
            bootstrap_delay_ms: None,
            preferences: None,
            startup_test_key: None,
        }))
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!(
            "device capture, GUI, and ProtoInput require Windows; profile validation is portable"
        )
    }
}

#[cfg(windows)]
fn windows_main(command: Command) -> Result<()> {
    let command = match command {
        Command::Installation { command } => return installation::run(command),
        Command::Config { command } => {
            let _installation_lease = installation::runtime_lease()?;
            return config_maintenance::run(command);
        }
        Command::Gui {
            renderer,
            data_dir,
            profile,
            ui_script,
            ui_script_no_screenshots,
            discovery_delay_ms,
            start_preflight_delay_ms,
            profile_file_delay_ms,
            app_preferences_delay_ms,
            bootstrap_delay_ms,
            preferences,
            startup_test_key,
        } => {
            return gui::run(
                data_dir,
                profile,
                ui_script,
                gui::GuiRunOptions {
                    renderer,
                    script_screenshots: !ui_script_no_screenshots,
                },
                preferences,
                startup_test_key,
                gui::WorkerDelays {
                    discovery: std::time::Duration::from_millis(discovery_delay_ms.unwrap_or(0)),
                    start_preflight: std::time::Duration::from_millis(
                        start_preflight_delay_ms.unwrap_or(0),
                    ),
                    profile_file: std::time::Duration::from_millis(
                        profile_file_delay_ms.unwrap_or(0),
                    ),
                    app_preferences: std::time::Duration::from_millis(
                        app_preferences_delay_ms.unwrap_or(0),
                    ),
                    bootstrap: std::time::Duration::from_millis(bootstrap_delay_ms.unwrap_or(0)),
                },
            );
        }
        command => command,
    };
    let _installation_lease = installation::runtime_lease()?;
    use mux_windows::{
        protoinput::Backend,
        raw_input::{self, Capture},
    };
    match command {
        Command::Installation { .. } | Command::Config { .. } | Command::Gui { .. } => {
            unreachable!()
        }
        Command::EmergencyBroker => emergency_stop::broker_main(),
        Command::DesktopCaptureFixture {
            report,
            max_seconds,
        } => desktop_capture_fixture::run(report, std::time::Duration::from_secs(max_seconds)),
        Command::Mcp {
            runtime,
            gui_connection,
            preferences,
            allow_app_preferences,
            allow_mapper_library,
            data_dir,
        } => {
            let runtime = runtime.unwrap_or_else(app_paths::runtime_dir);
            let custom_document = preferences.is_some();
            let preferences = app_paths::preferences_path(data_dir, preferences)?;
            let root = preferences
                .parent()
                .context("preferences path has no parent")?;
            let _data_lease = if custom_document && !root.exists() {
                // An explicitly chosen, absent document keeps its read-only
                // discovery behavior without creating its parent directories.
                None
            } else {
                config_maintenance::runtime_lease(root)?
            };
            let flow_store = allow_mapper_library.then(|| mapper_flow_store::Store::new(root));
            mcp::run(
                runtime,
                gui_connection,
                Some(preferences),
                allow_app_preferences,
                flow_store,
            )
        }
        Command::Probe {
            report,
            automation,
            capture_pattern,
            cursor_fixture,
            keyboard_focus_fixture,
            native_controls_fixture,
            accelerator_fixture,
            keyboard_state_fixture,
        } => {
            if automation {
                automation_probe::run(
                    report,
                    capture_pattern,
                    cursor_fixture,
                    keyboard_focus_fixture,
                    native_controls_fixture,
                    accelerator_fixture,
                    keyboard_state_fixture,
                )
            } else {
                anyhow::ensure!(
                    !native_controls_fixture,
                    "native controls fixture requires --automation"
                );
                anyhow::ensure!(
                    capture_pattern.is_none() && !cursor_fixture && !keyboard_focus_fixture,
                    "capture/cursor fixtures require --automation"
                );
                probe::run(report)
            }
        }
        Command::SessionWorker {
            runtime,
            controller_pid,
        } => session::run(&runtime, controller_pid),
        Command::Devices => {
            println!("{}", serde_json::to_string_pretty(&raw_input::devices()?)?);
            Ok(())
        }
        Command::Validate { profile, .. } => {
            let resolved = read_profile(&profile)?.resolve(&raw_input::devices()?)?;
            println!("Valid profile: {} connected seat(s)", resolved.len());
            Ok(())
        }
        Command::Doctor { runtime } => {
            let runtime = runtime.unwrap_or_else(app_paths::runtime_dir);
            let backend = Backend::load(&runtime)?;
            println!(
                "{} loaded; all required API exports resolved. Target-specific runtime files are checked on attach.",
                Backend::loader_name()?
            );
            println!(
                "Virtual session lifecycle ABI: {}. Resident target compatibility and cleanup commitment are checked separately before reuse.",
                backend.lifecycle_version()
            );
            Ok(())
        }
        Command::Observe { seconds } => {
            use std::time::{Duration, Instant};
            let capture = Capture::start()?;
            let start = Instant::now();
            let duration = Duration::from_secs(seconds.min(3600));
            let mut packets = 0u64;
            while let Some(remaining) = duration.checked_sub(start.elapsed()) {
                match capture.events.recv_timeout(remaining) {
                    Ok(raw_input::Event::Mouse { .. }) => packets += 1,
                    Ok(raw_input::Event::Keyboard { .. }) => (),
                    Ok(raw_input::Event::Error(e)) => anyhow::bail!("{e}"),
                    Ok(raw_input::Event::DevicesChanged) => println!("Device inventory changed"),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(e) => return Err(e.into()),
                }
            }
            println!(
                "Observed {packets} mouse packets; {} dropped",
                capture.dropped_packets()
            );
            Ok(())
        }
        Command::Attach { profile, runtime } => {
            let runtime = runtime.unwrap_or_else(app_paths::runtime_dir);
            let profile = read_profile(&profile)?;
            anyhow::ensure!(
                !profile.concurrent && profile.desktop.is_none(),
                "Concurrent profiles require the supervised GUI session"
            );
            let seats = profile.resolve(&raw_input::devices()?)?;
            let session = Backend::load(&runtime)?.attach(&seats)?;
            println!(
                "Attach/configuration requests sent for {} seat(s). Press Enter to request hook removal. Restart targets before reattaching.",
                session.instance_count()
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            drop(session);
            Ok(())
        }
    }
}

#[cfg(test)]
mod renderer_cli_tests {
    use super::*;

    #[test]
    fn gui_renderer_is_explicit_and_defaults_to_glow() {
        let parsed = Args::try_parse_from(["protoinputmultiplex", "gui"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Gui {
                renderer: GuiRenderer::Glow,
                ..
            })
        ));
        let parsed =
            Args::try_parse_from(["protoinputmultiplex", "gui", "--renderer", "software-dx12"])
                .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Gui {
                renderer: GuiRenderer::SoftwareDx12,
                ..
            })
        ));
    }

    #[test]
    fn renderer_option_rejects_typos_and_does_not_apply_to_probe() {
        assert!(Args::try_parse_from(["protoinputmultiplex", "gui", "--renderer", "cpu"]).is_err());
        assert!(
            Args::try_parse_from([
                "protoinputmultiplex",
                "probe",
                "--renderer",
                "software-dx12",
            ])
            .is_err()
        );
    }
}
