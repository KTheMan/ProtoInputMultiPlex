# Two-game MCP demo

https://github.com/user-attachments/assets/b0612af3-7fbf-4056-a4b8-8598f72d97fd

This 21.53-second video shows two virtual mouse-and-keyboard users controlling
Mines and Fifteen in separate owned processes on VM105. The unchanged
ProtoInputMultiPlex RC starts routing through the GUI. Agent Access grants two
MCP clients input to the selected users and exact game PIDs; capture and
session-change permissions stay off.

The clients replay prepared moves on a shared clock. They are not AI agents
solving the games. Mines receives flag clicks; Fifteen completes and undoes moves.

## Movement method

The external demo generator uses the Cursory library and its 2,356 recorded human
movements through [Camoufox's planner](https://github.com/daijro/camoufox/blob/fdea92335b97ae863e9f7bfc218f7bed2043546d/browser/additions/juggler/input/CursorTrajectory.js),
pinned to commit `fdea92335b97ae863e9f7bfc218f7bed2043546d`.
The demo adapts the planner's nominal sampling rate from 60 to 30 Hz to fit the
existing RC's measured request budget. It preserves recording choices and
movement durations, uses fixed seeds and private high-resolution timers, and
coalesces only overdue intermediate moves. Endpoints, buttons and keys remain
mandatory.

This is a demo input generator, not a new movement option built into the RC.
Camoufox's Firefox dispatcher is not used. The generated points are delivered
through the RC's public `routing_input` API. This verifies the generator and live
delivery, not indistinguishability from a person.

## Capture and verification

The MP4 is a mixed-rate live window/cursor composition at normal speed: game
PrintWindow snapshots at 1.91 Hz and observed native cursor positions at 30.02 Hz,
encoded at 30 fps. Native cursor bitmaps were captured at start/end and
hash-matched; their positions come from actual HWND measurements, not authored
paths. There is no video motion interpolation. The footage starts with the first
available composite after asynchronous capture initialization; it is not a
frame-complete recording of both script loops or desktop frame-rate/latency proof.

A separate observer, receiving no authored path or replay clock, tracked native
cursor positions at 102.66 Hz. Independent review accepted the run and checked:

- All 26 generated moves and both input clips against the pinned generator.
- All 52 exact endpoint visits across two repeats, plus overlapping native motion.
- 285 / 296 acknowledged input events, with 15 / 38 overdue intermediate moves
  explicitly coalesced and all mandatory actions preserved.
- Actual Mines flag changes and Fifteen completion/undo.
- All 693 source JPEGs and all 646 encoded video frames.
- All 45 Defender samples protected, no recorded alerts or scoped crashes,
  normal exit of three owned apps and two MCP clients, both test tasks removed,
  and no remaining scoped processes.

Workstation input, RustDesk and vendor sources were untouched. This run covers
these two processes and virtual devices; it does not establish hardware support,
arbitrary-application compatibility or desktop-wide/same-process isolation.

## Credits and identity

Games: [Simon Tatham's Portable Puzzle Collection](https://www.chiark.greenend.org.uk/~sgtatham/puzzles/).
Movement: [Camoufox](https://github.com/daijro/camoufox) and
[Cursory](https://github.com/Vinyzu/cursory), via
[cursory-js](https://github.com/JWriter20/cursory-js).
The upstream recording credits include SapiMouse and Mouse-Synthesizer;
[Camoufox's Cursory notice](https://github.com/daijro/camoufox/blob/fdea92335b97ae863e9f7bfc218f7bed2043546d/browser/additions/juggler/input/cursory/NOTICE)
records provenance and licensing. Native hooking uses
[Ilyaki's ProtoInput](https://github.com/Ilyaki/ProtoInput).

| Item | Identity |
| --- | --- |
| RC source | `9733060d99187d82d6ad7bdc2666d9dce988db15` |
| Inner EXE SHA-256 | `b61a1047ed57de06745284d254d0e3d082d67dbfc1de4c443e73d37982b640b4` |
| MP4 SHA-256 | `578969b52a6f0af5505cc8380993f939b4f8af234d70f269c8d58d906b3b8bd2` |
| Retained VM evidence archive SHA-256 | `7a0161747be8f5c92cb51632b0f8297c51170c785d7e69cce4ce7d92bdb01b4e` |

The evidence archive is retained with the qualification materials; it is not
attached to this public README. The portable EXE and installer are unchanged.
