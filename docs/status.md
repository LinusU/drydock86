# Status

This document records what Drydock implements, what has been verified and how, and
what is known to be missing. It is updated as work lands; the newest verification
results are at the top of each section.

## Acceptance workload

The development workload is the retail *Midtown Madness 2* (US English, PE32 GUI
executable, image base 0x400000, relocations stripped, statically linked MSVC
runtime; executable SHA-256 `93afb6c0…3f98a2`). Drydock contains no knowledge of it:
the installation path, scenarios, recordings and screenshots are local data under the
ignored `local/` directory and are used through the generic CLI and protocol.

### Verified retail paths

All on macOS 26, Apple M5 Max, Metal, default profiles (Windows 2000 SP4, 800 MHz /
800 MIPS guest CPU, 640×480×16 display), unmodified game files, no debugger
mutations.

| Gate | Result | Evidence (local) |
| --- | --- | --- |
| Retail launch | The executable starts from the installation; EULA dialog (RichEdit text) appears and is accepted with ordinary input | `local/scenarios/eula.json` |
| Menus and loading | Main menu, driver profiles, race and vehicle selection with 3D vehicle preview, loading screens, quit prompt | `local/scenarios/menu1.json`, `race1.json` |
| Intro movie | `logos.avi` (Indeo 5, 320×240, 15 fps, 332 frames, 8-bit mono PCM) plays centred in its MCIWnd window before the splash screen: 332 frames presented over 22.13 s of virtual time, the sound track is in the WAV capture, and a mouse click stops it at the game's next poll (the game checks every 250 ms) | `intro.json`, `intro_skip.json`; screenshots `i1–i3`; `audio.wav` |
| Interactive gameplay | Quick Race "Go Drive" in San Francisco: city, traffic, car model, HUD, radar map, sky; steering, acceleration and braking respond; the audio capture contains continuous engine and effect sound while driving (checked by level, not judged by ear) | `drive1.json`; screenshots `d0–d2`; `audio.wav` |
| Additional gameplay path | Blitz race "Ignorance Is Blitz": briefing/loading, checkpoint counter and countdown, race ends with "Time's up!", Restart Race from the pause menu resets the race, Quit to Race Menu returns to the Races screen | `race1.json`, `race3.json` |
| Persistence | A driver profile created in the game's dialog and saved on quit through the game's own Quit prompt is selected again after restarting Drydock with the same overlay | `persist2.json`, `persist3.json` |
| Headless gameplay | All of the above ran headless (offscreen wgpu, no window, discard or WAV audio) | reports in `local/artifacts/*` |
| Device equivalence | Replaying a recorded input timeline in the desktop player executes the same number of guest instructions and presents the same number of frames as the headless run (10,721,602,760 instructions, 1226 frames for `drive1`, measured before the intro movie played; with the movie the headless run is 10,721,797,621 instructions and 1558 frames); frames differ only by RGB565 quantisation (≤ 7 per channel) because the player shows the GPU image directly | `play-eq*` |
| State restoration | A mid-race savepoint, a disk checkpoint restored in the same process, and the same checkpoint restored in a fresh process all produce bit-identical frames after the same 5 s input sequence | `ckpt1.json`, `ckpt_resume.json` |
| Replay | Replaying `drive1`'s recorded input timeline without the scenario reproduces the instruction count, frame count and final frame exactly | `replay1` |
| Agent usability | Protocol sessions against the game: bounded runs, cancel of a 600 s run after 2 s, screenshots, state, API breakpoints with memory and register inspection | `local/proto_smoke.py`, `local/proto_lock.py` |
| Sustained run | 676.5 s (11.3 min) of virtual time in one session: menus, then continuous driving with scripted accelerate/reverse/steer cycles through different parts of the city; 39,947 frames, no halt, audio present in every minute of the capture, no unimplemented-call stops | `soak2.json`, per-minute screenshots, `audio.wav` |

Not verified: completing a race through its checkpoints (the scripted inputs are
open-loop), other race types, multiplayer, the other city, every vehicle, options
beyond those visited.

Observation from these runs: the installation directory contains a stale `MM2.lock`
left by earlier use. When the game finds one at start-up it deletes it, tries to open
`Trouble.rtf` with `system("start …")` (which fails: the guest has no command
interpreter), and exits. A fresh overlay therefore takes this path once. Runs that end
by time limit leave the game's lock in a persisted overlay in the same way a power
cut would; use `--no-persist` for experiments.

### Second workload: Crimson Skies

Retail *Crimson Skies* 1.02 (Zipper Interactive, 2000; MFC, DirectDraw 4 /
Direct3D 6, DirectShow movies), launched from an unpacked copy of the executable
(`--exe` outside `--root`), with the Windows fonts the game uses mapped into the
guest (`--map 'C:\WINNT\Fonts=…'`). Verified headless on the same host:

| Path | Result |
| --- | --- |
| Start-up | EULA, the DirectX version warning (answered Ignore: the game asks for DirectX 8, Drydock is DirectX 7), the game's "Select Video Device" dialog, splash screen, Microsoft/Zipper/Aero-Tone MPEG-1 movies with sound |
| Menus | Main menu, Preferences, Credits, Multiplayer options, pilot selection; all text drawn with the game's TrueType fonts (Andy Bold, Trebuchet MS, Bell MT, …) |
| Instant Action | Mission list and drop-down selections, briefing, 3D flight at 1024×768 with terrain, aircraft, sky, fog, rain, HUD and gauges; keyboard flight controls and guns respond; Esc pause menu; Quit leads to the debriefing |
| Campaign | Pilot selection, intro movie (skipped with Esc), hangar, animated briefing map with voice-over, Flight Check, first mission's launch sequence from the Pandora |
| Joystick and force feedback | With `--joystick` (SideWinder Force Feedback 2), scripted joystick input in Instant Action: the throttle slider sets engine power (207 mph at full, 61 mph at idle), the stick banks the plane, button 1 fires the guns and button 2 a rocket. The game hands the joystick to Immersion's IFC library (`ifc21.dll`), which creates the 16 effects of `crimsonff.ifr` (constant forces with envelopes, sawtooth, sine and triangle rumbles) with the parameters in that file; the gun effect starts and stops with the trigger and rockets play their kick, as recorded by `--force-feedback-log` |
| Speed | Menus faster than realtime. In flight the game renders 60 frames per virtual second with about 800 draw calls each and transforms and lights vertices itself on the x87; headless unthrottled runs it at about 0.7× realtime (15 s in 21.6 s), and the desktop player runs flight in slow motion (about 0.6×, dropping the lag) |

With a real SideWinder Force Feedback 2 on macOS (USB, no driver): its report
descriptor is byte-identical to the captured one the tests use; `drydock joystick
--test-force-feedback` runs every step without error and the stick's own PID state
reports confirm each effect playing and stopping; in the player, a restored flight
checkpoint downloads the game's 16 effects to the stick and gunfire and a rocket
start theirs, with no device errors. Not verified: how the forces feel (the stick
applies force only with a hand on the grip and nobody held it), stick position input
(nobody moved it; the stick stalls requests for its current state, so the throttle is
known only once moved), Linux hosts.

Not verified: completing a mission, later campaign missions, multiplayer (DirectPlay
is unavailable), mouse flight, saving and loading campaign progress across runs. Known defect: on the debriefing screen, the second line of a label the
game wraps (such as "Danger Zones Completed") is drawn into the text bitmap but not
shown.

## Implemented platform

- **Execution:** Unicorn 2.1.5 (pinned), Pentium III CPU model, flat segments,
  per-thread TEB through FS, x87/SSE state saved per thread, precise faults, SEH
  with unwinding and unhandled-exception filters, self-modifying code, lazily
  executable data pages (no DEP).
- **Loader:** PE32 with relocations, imports by name and ordinal, exports and
  forwarders, resources, TLS; runtime `LoadLibrary`/`GetProcAddress`; DLL entry
  points; Windows search order for guest DLLs; builtin system DLLs as real PE images.
- **kernel32:** memory (Virtual*, heaps, Global/Local), files and directories through
  the VFS with share modes and enumeration, threads, events, mutexes, semaphores,
  critical sections, waits with timeouts, TLS, time and performance counters, NLS and
  code pages (cp1252), environment, module and process queries, debug output.
- **user32/GDI:** windows, classes, message queues, posted/sent messages, timers,
  input messages, focus and activation, dialogs (dialog templates, buttons, statics,
  edit, combo box, list box, RichEdit with RTF text), message boxes, cursors, display
  modes, menus, accelerators, window hooks (CBT); GDI DCs, bitmaps, DIB sections,
  palettes, BitBlt/StretchBlt with raster operations, fonts and text output
  (DrawTextEx with margins and partial drawing) rasterized with the TrueType fonts
  in the guest's Fonts folder when a face is installed there, else with the bundled
  Liberation fonts.
- **Other system DLLs:** INI profiles, FormatMessage, date and time formatting,
  version resources (version.dll), MS ADPCM and IMA ADPCM decoding (msacm32), COM
  activation, GUID strings, CoCreateGuid (reproducible) and the task allocator (ole32).
- **advapi32:** registry with `.reg` import/export, persisted in the overlay.
- **DirectDraw 7 / 4 / 2 / 1:** display modes, primary and flip chains with vsync-retired
  flips, offscreen and texture surfaces in system/video memory, mip chains, locks,
  Blt/BltFast with colour keys and stretching, palettes, clippers, gamma ramps, GetDC,
  EnumSurfaces, DXT1/3/5 textures.
- **Direct3D 7:** HAL, TnL HAL and RGB devices; transforms, lights, materials,
  viewports, render states, two texture stages, texture coordinate generation and
  transforms, alpha test and blending, fog, depth (16/24/32-bit), culling, colour
  keying, vertex buffers, DrawPrimitive/DrawIndexedPrimitive (and VB variants),
  state blocks, Device7::Load.
- **Direct3D 6:** IDirect3D3, Device3 (legacy texture handles and blend modes),
  Viewport3 with lights and background material, Material3, Texture2, vertex
  buffers. Device3 leaves the x87 precision unchanged, as games of that era convert
  colours with the 53-bit rounding trick.
- **DirectShow:** IAMMultiMediaStream playback of MPEG-1 system streams to a
  DirectDraw surface with audio (amstream).
- **DirectPlay lobby:** reports that the program was not started by a lobby.
- **DirectInput 7:** keyboard, mouse, joystick; buffered and immediate data,
  acquisition and cooperative levels, dead zone and saturation, COM activation and
  CreateDeviceEx. The joystick is a SideWinder Force Feedback 2 (X, Y, twist,
  throttle slider, 8 buttons, hat) with force feedback: the standard constant, ramp,
  periodic and condition effects with envelopes, start delay and gain; download,
  start/stop and status on virtual time; device commands, state, gain and
  auto-centre. Custom forces are not offered. In the player a host game controller
  drives it: joysticks through HID (USB PID force-feedback controllers play the effects),
  Xbox-style gamepads through gilrs (no force feedback).
- **DirectSound:** primary and secondary buffers, duplication, play/write cursors on
  virtual time, looping, notifications, volume/pan/frequency, 3D listener and buffers
  (distance attenuation, panning and Doppler are mixed; cone parameters are stored but not applied), software mixing to device, WAV or discard.
- **winmm:** timeGetTime, timer resolution, multimedia timers with callbacks,
  joystick, MCI command dispatch (CD audio reports no device).
- **msvfw32:** MCIWnd windows playing AVI files on the virtual clock (Indeo 5 video,
  PCM audio): MCIWndCreate, play, pause, resume, stop, mode, length and position
  queries.
- **DirectMusic:** performance, ports, loader and composer objects with their
  queries and settings.

## Known gaps

Behaviour the acceptance workload reaches but Drydock does not implement. Each is
reported at run time as a structured diagnostic, never as fake success.

- **Video for Windows:** only MCIWnd with Indeo 5 video and PCM audio. Other codecs
  and audio formats fail to open with a "missing" diagnostic; AVIFile, DrawDib and
  the installable-compressor (IC*) APIs are not provided, nor are MCIWnd's play bar,
  menu, notifications and seeking.
- **DirectMusic playback:** creating segments (`CLSID_DirectMusicSegment`) is not
  implemented and no General MIDI DLS collection is available (`gm.dls` is a Windows
  system file); the game runs without music. CD audio is unavailable because no
  CD-ROM drive can be mapped.
- **DirectPlay:** `DirectPlayCreate` reports `DPERR_UNAVAILABLE`; multiplayer is
  unavailable.
- **Direct3D:** stencil buffers, wireframe/point fill modes, more than two texture
  stages, user clip planes, ProcessVertices; texture coordinate generation beyond
  camera-space normal/position/reflection.
- **DirectDraw:** overlays, DuplicateSurface, SetSurfaceDesc, GetSurfaceFromDC.
- **Processes:** no child processes (`CreateProcess` of guest programs, `system()`);
  no `cmd.exe`.
- **Fidelity:** x87 arithmetic is Unicorn's softfloat; transcendental instructions
  have not been compared against hardware. Virtual time counts instructions, not
  cycles. No comparison against a real Windows run has been made; historical
  fidelity is unverified. Reproducibility is established only on the same build,
  host and backend.

## Performance

Measured on an Apple M5 Max shared with other projects' parallel builds (load
average 3–5). Under that load the same run lands in one of two host speed states,
about 1.4× apart for identical work (CPU time scales the same way, which points at
core placement or frequency rather than Drydock); figures below are the best of
several runs, i.e. the faster state, unless noted. Guest instruction counts are
identical across all runs of a workload.

| Workload | Guest time | Host time | Speed |
| --- | --- | --- | --- |
| `drive1` (menus, loading, Quick Race driving), headless unthrottled | 31.06 s | 23.4 s | 1.33× |
| Driving, 8.86 s from a race-start checkpoint, headless unthrottled | 8.86 s | 7.30 s | 1.21× |
| Same, slow host state | 8.86 s | 13.0 s | 0.68× |
| Same, tracing every API call into a 10,000-entry ring (`--trace !`) | 8.86 s | | within 2 % of untraced |
| Same segment in the desktop player (realtime pacing, device audio) | 8.86 s | 9.7 s | 0.92× |
| `drive1` input replayed in the desktop player (realtime pacing) | 31.06 s | 39.5 s | 0.79× |
| 11-minute soak (`soak2`, mostly driving, WAV capture), before patch 0002 | 676.5 s | 509 s | 1.33× |

How the speed is made up: in the race the game is idle (waiting for vertical blank)
about 40 % of guest time, which unthrottled runs skip. The busy part runs at about
0.73× realtime in the fast host state and 0.41× in the slow one. Realtime pacing
recovers lag during idle gaps, so play keeps realtime as long as busy-phase speed
stays above the game's busy fraction (about 0.6); this holds in the fast state on
this host and fails in the slow one, where the player runs in slow motion.
Longer busy stretches, such as loading, always run slower than realtime; realtime
pacing drops lag beyond 100 ms rather than fast-forwarding afterwards (reported as
`pacing.dropped_ns`). **Interactive play is close to, but not reliably at, full
speed on this host.**

Host profile of driving (fast state): about 80 % inside Unicorn (generated code,
softfloat x87 helpers, the stop check at each block start, indirect-jump lookups),
about 10 % in Direct3D translation and frame submission, the rest in API dispatch.
The vendored Unicorn carries two patches (`vendor/README.md`) that keep guest stores
on the fast path and drop a helper call after every memory access; together with a
plain C instruction-counting hook they took this benchmark from 0.53× to 1.21×.
The remaining large lever is the software TLB: stores across more than about 256
pages run at a seventh of the speed of stores within a few pages
(`bench_stores_across_pages`); a larger initial TLB did not help the game.

Crimson Skies flight (15 s of guest time from `local/cs/art/flight/flight.ddck`, headless,
same instruction count 14,887,848,064 and 1724 frames before and after; host load 5–7, so
figures are noisy): before the Unicorn patches 0004–0007 the segment took 24–25 s of wall
time in the best interleaved runs (0.6× realtime); after them 15–18 s in quiet moments and
21.7 s at the same load as the 25 s runs (0.7–1.0×). What changed: inline block counting
instead of a hook (0004), host-double x87 add/sub/mul/div/FSTP m32 and a direct FLD m32
(0005), inline exit-request test and no per-block JIT write toggling (0006), and a minimum
software TLB of 4096 entries (0007; 93 million slow-path loads in 15 s fell to 1 million).
Midtown Madness 2 `local/bench.sh 3`: 1.07× → 1.13–1.27×; `drive1` has identical
instructions (10,721,740,919) and frames. Remaining profile: x87 helpers and FLD from
cache-cold guest data, ~15 % API dispatch (a depth clear flushes the Direct3D batch).

Indeo 5 decoding of the 332-frame intro movie (320×240) takes about 85 ms in
total on the host (`cargo run --release -p drydock-media --example avi_frames`).

Micro-benchmarks (`cargo test --release -p drydock-core --test bench -- --ignored
--nocapture`): tight integer loop ≈ 1350 MIPS, call/ret with stores ≈ 590 MIPS, API
trap round trip ≈ 0.46 µs.

Savepoints in a race: create 53 ms, restore 26 ms; disk checkpoint 13–24 MB written
in 0.38 s and restored in 0.15 s.

Linux (aarch64 container on the same machine, Lavapipe software Vulkan): `drive1`
headless ran at 0.57× (before the Unicorn patches) with the same guest instruction
and frame counts as on macOS; loading-screen frames were pixel-identical, rasterized
race frames differ between Lavapipe and the Apple GPU.

## Open items

1. Interactive speed (see Performance): busy-phase speed needs more headroom.
   Crimson Skies flight is the heaviest case so far: about 84 % of host time is
   emulation, dominated by softfloat x87 helpers.
   Candidates: Unicorn's software-TLB behaviour on large working sets, the per-block
   stop-check helper, moving Direct3D translation and frame submission to a render
   thread.
2. DirectMusic segments and DLS synthesis; CD-ROM drive mapping for CD audio.
3. CI has not run yet (the workflow is untested); Linux verification so far is a
   local aarch64 container.
4. Licence compatibility of Apache-2.0-only dependencies with GPL-2.0-only Unicorn
   for binary distribution (see THIRD_PARTY_NOTICES.md).
5. Child processes.
