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
| Interactive gameplay | Quick Race "Go Drive" in San Francisco: city, traffic, car model, HUD, radar map, sky; steering, acceleration and braking respond; the audio capture contains continuous engine and effect sound while driving (checked by level, not judged by ear) | `drive1.json`; screenshots `d0–d2`; `audio.wav` |
| Additional gameplay path | Blitz race "Ignorance Is Blitz": briefing/loading, checkpoint counter and countdown, race ends with "Time's up!", Restart Race from the pause menu resets the race, Quit to Race Menu returns to the Races screen | `race1.json`, `race3.json` |
| Persistence | A driver profile created in the game's dialog and saved on quit through the game's own Quit prompt is selected again after restarting Drydock with the same overlay | `persist2.json`, `persist3.json` |
| Headless gameplay | All of the above ran headless (offscreen wgpu, no window, discard or WAV audio) | reports in `local/artifacts/*` |
| Device equivalence | Replaying a recorded input timeline in the desktop player executes the same number of guest instructions and presents the same number of frames as the headless run (10,721,602,760 instructions, 1226 frames for `drive1`); frames differ only by RGB565 quantisation (≤ 7 per channel) because the player shows the GPU image directly | `play-eq*` |
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
  modes; GDI DCs, bitmaps, DIBs, BitBlt/StretchBlt, fonts and text output rasterized
  with the bundled Liberation fonts.
- **advapi32:** registry with `.reg` import/export, persisted in the overlay.
- **DirectDraw 7 / 1:** display modes, primary and flip chains with vsync-retired
  flips, offscreen and texture surfaces in system/video memory, mip chains, locks,
  Blt/BltFast with colour keys and stretching, palettes, clippers, gamma ramps, GetDC,
  EnumSurfaces, DXT1/3/5 textures.
- **Direct3D 7:** HAL, TnL HAL and RGB devices; transforms, lights, materials,
  viewports, render states, two texture stages, texture coordinate generation and
  transforms, alpha test and blending, fog, depth (16/24/32-bit), culling, colour
  keying, vertex buffers, DrawPrimitive/DrawIndexedPrimitive (and VB variants),
  state blocks, Device7::Load.
- **DirectInput 7:** keyboard, mouse, joystick; buffered and immediate data,
  acquisition and cooperative levels.
- **DirectSound:** primary and secondary buffers, duplication, play/write cursors on
  virtual time, looping, notifications, volume/pan/frequency, 3D listener and buffers
  (distance attenuation, panning and Doppler are mixed; cone parameters are stored but not applied), software mixing to device, WAV or discard.
- **winmm:** timeGetTime, timer resolution, multimedia timers with callbacks,
  joystick, MCI command dispatch (CD audio reports no device).
- **DirectMusic:** performance, ports, loader and composer objects with their
  queries and settings.

## Known gaps

Behaviour the acceptance workload reaches but Drydock does not implement. Each is
reported at run time as a structured diagnostic, never as fake success.

- **Video for Windows / Indeo 5:** `MSVFW32.DLL` is not provided, so
  `LoadLibrary` fails and the game skips its intro movie `logos.avi` on its own.
  Implementing MCIWnd/AVIFile and an Indeo 5 decoder is open.
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

Measured on an Apple M5 Max that was simultaneously running other projects' builds
(load average 3–4), which makes single measurements vary by up to ±20 %; figures are
best of three unless noted.

| Workload | Guest time | Host time | Speed |
| --- | --- | --- | --- |
| Menus and loading (`drive1` steps 0–4) | 11.1 s | 4.9 s | 2.3× |
| Driving, 9 s from a race-start checkpoint (unthrottled, no audio) | 8.86 s | 16.7 s | 0.53× |
| Same, tracing every API call into a 10,000-entry ring (`--trace !`) | 8.86 s | 17.0 s | 0.52× (identical guest instruction count) |
| Interactive player, `drive1` input replayed, device audio | 31.1 s | 57.8 s | 0.54× overall |
| Sustained soak (`soak2`, mostly driving, WAV capture) | 676.5 s | 915 s | 0.74× overall |

Driving is CPU-bound in Drydock: the game executes roughly 500 million guest
instructions per virtual second there (about 64 % of the 800 MIPS profile). Host
profile of the driving segment: about 75 % in Unicorn (generated code, softfloat x87
helpers, the exit check Unicorn inserts after every guest memory access, slow-path
stores), about 7 % in Direct3D draw translation, 7 % submitting frames to wgpu, the
rest in API dispatch and device servicing. Unicorn's software TLB is the main
limit on store throughput: stores slow down by half once a loop touches more than
about 256 pages (`bench_stores_across_pages`); the TLB only grows when flushed, and
periodic flushing did not help the game's workload in matched measurements. Interactive play therefore runs at about
half speed during races (realtime pacing slows the game rather than skipping time);
**interactive playability at full speed is not achieved** at the default CPU profile
on this host.

Micro-benchmarks (`cargo test --release -p drydock-core --test bench -- --ignored
--nocapture`): tight integer loop ≈ 386 MIPS, API trap round trip ≈ 0.37 µs,
call/ret with stores ≈ 104 MIPS.

Savepoints in a race: create 53 ms, restore 26 ms; disk checkpoint 13–24 MB written in
0.38 s and restored in 0.15 s.

## Open items

1. Interactive speed in races (see Performance): moving draw translation and frame
   submission to a render thread, cheaper per-block instruction counting, and
   reducing Unicorn's per-access overhead are the candidates.
2. Video for Windows (AVIFile, MCIWnd) and an Indeo 5 decoder for the intro movie.
3. DirectMusic segments and DLS synthesis; CD-ROM drive mapping for CD audio.
4. Linux headless verification, including Lavapipe; CI.
5. Licence compatibility of Apache-2.0-only dependencies with GPL-2.0-only Unicorn
   for binary distribution (see THIRD_PARTY_NOTICES.md).
6. Child processes.
