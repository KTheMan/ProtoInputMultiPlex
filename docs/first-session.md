# A first session with virtual users

This walkthrough starts from a fresh portable extraction or a fresh installed
profile using the default assigned concurrent route. It uses virtual mouse and
keyboard pairs and the application's own routing test windows.

The built-in test windows use OpenGL, including with the software-DX12 dashboard.
This walkthrough needs a working OpenGL driver for those windows. If a test
window cannot open, use controlled applications selected from **Output**, with
a distinct target process for each user. The dashboard's software-DX12
qualification used separate native automation probes.

## Open the application

For the portable EXE, select a new extraction folder and launch from Finish.
Use `launch-gui.cmd` when opening it again. For an installation, launch
ProtoInputMultiPlex from the Start menu.

Keep the complete extracted or installed bundle together. The executable uses
the accompanying `runtime` directory and other packaged resources.

## Set up two users

1. Open **Settings → Users & devices** and choose **Add emulated user**.
   This creates a virtual mouse and keyboard pair and opens that user's settings.
2. In the user's **Output** tab, choose **Open routing test window**.
   The application creates and selects a disposable test process for that user.
3. Return to **Users & devices**, add a second emulated user, and open its own
   routing test window from **Output**. Each user should have a different target
   process.
4. Return Home and click **Start routing**. Check the displayed readiness and
   routing status before delivering input.

## Deliver input

Open **Settings → Tools → Input lab**. Use each user's mouse and keyboard
controls and watch its corresponding test window. The virtual controls operate
their own users while the normal desktop pointer remains available.

For example, use **1 Right** to move the first user's pointer and **1 Tap W**
to send a key, then try the second user's equivalent controls. The test window's
**Cursor API** and **Held test keys** displays provide visual feedback.

## Stop the session

Return Home and click **Stop routing**, then close the two test windows when
finished. The default **Ctrl+Alt+Shift+F11** emergency shortcut stops subscribed
sessions using that shared Stop binding. See the [emergency Stop contract](shared-emergency-stop-contract.md)
for its scope and recovery behavior.

Use any readiness/repair feedback shown by the application before starting
another session.

## Continue from here

- [Desktop routing](concurrent-routing.md) explains automatic click-selected
  targeting and switched/concurrent modes. Configure an explicit eligible-PID
  list when you want to restrict automatic targeting to chosen processes.
- [RC agent setup](https://github.com/KTheMan/ProtoInputMultiPlex/blob/d38f5676b81568f024d7ea989b883cf9f407f04a/docs/agents.md) explains controlling virtual users from MCP and the
  separate grants used by a GUI connection.
- [Installation and recovery](installation-recovery.md) covers data locations,
  profiles, backups and uninstalling.
- [Known limits](known-limitations.md) describes the current compatibility and
  validation scope.

The published RC's independent VM105 test used two owned automation probes and
the live GUI's automatic routing, mode-change, keyboard Apply and Stop paths.
This assigned-route walkthrough describes the built-in product controls. Its
instructions were checked against the released source and existing routing
guide during the documentation rewrite.
