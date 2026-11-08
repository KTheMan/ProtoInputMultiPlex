ProtoInputMultiPlex portable (Windows x64)

Extract the entire folder to a writable location, then run launch-gui.cmd.
Keep protoinputmultiplex.exe, runtime, licenses and source together. No install,
administrator step, automatic startup registration or updater is added.
The x64 app includes native runtime support and cursor observers for x64 and x86 targets.
See INSTALLATION.md for Windows setup, upgrade, migration and recovery commands.

Launchers set the working directory to this folder. GUI runtime is runtime;
profiles default to data/profiles/local.json and macros to data/macros/automation.json.
Application preferences use data/app-preferences.json (shared by both launchers).
Those files are created on explicit saves/configuration; there is no initial
user configuration in this bundle. Keep this folder writable and preserve data,
profiles and macros when replacing binaries. Import historical sibling profiles/macros
with config migrate --legacy-root (see INSTALLATION.md). Avoid simultaneous writers.

MCP clients can launch cmd.exe with arguments:
  /d /c "C:\path with spaces\ProtoInputMultiPlex\launch-mcp.cmd"
MCP uses stdio. Keep stdout exclusively for the protocol. Optional MCP arguments
may follow the wrapper path, e.g. --gui-connection followed by the private GUI
descriptor path. No write grant is added by the wrapper. Desktop routing still
requires explicit target selection and an allowed PID list in standalone MCP.

Direct launches resolve their bundled runtime relative to the executable:
  protoinputmultiplex.exe mcp --runtime "C:\bundle\runtime" --preferences "C:\bundle\data\app-preferences.json"
  protoinputmultiplex.exe doctor --runtime "C:\bundle\runtime"
Direct GUI launches resolve profiles/macros beneath the chosen preferences root.
Direct launches without --preferences use
%LOCALAPPDATA%\ProtoInputMultiPlex\app-preferences.json. Direct double-click
on the EXE uses that default; prefer launch-gui.cmd for portable data.

Discovery and preview work without the ProtoInput DLLs. Doctor verifies the
loader and required exports without injecting; target-specific files are
checked on attachment. Missing runtime errors name the folder or DLL. A doctor
success does not validate injection/routing or every target application's DLL
search behavior. Windows system libraries and a working OpenGL driver remain
host requirements. No routing validation is implied by a successful launch.

bundle-manifest.json records SHA-256 hashes, source/build provenance, static
and delay imports, and host-system dependencies. It excludes itself and future
user data. licenses contains existing third-party notices. source contains the
native source/build inputs used for this bundle; some upstream components have
GPL/LGPL notices, so this distribution is not described as MIT-only. Consult
those notices and corresponding source before external redistribution.
