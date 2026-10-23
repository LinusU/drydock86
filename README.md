# Drydock

Drydock runs original 32-bit Windows games natively on a modern host. The guest's
x86 code executes unmodified inside [Unicorn](https://www.unicorn-engine.org/); the
Windows and DirectX environment it talks to (kernel32, user32, GDI, DirectDraw,
Direct3D 7, DirectInput, DirectSound, DirectMusic, MCI, …) is implemented in Rust;
graphics are rendered with [wgpu](https://wgpu.rs/). The same runtime drives an
interactive desktop window, headless batch runs, and a JSON control protocol for
agents that investigate program behaviour.

Drydock contains no knowledge of any particular program. The acceptance workload
during development is the retail release of *Midtown Madness 2*; everything specific
to it (installation path, input recordings, screenshots, notes) lives in an ignored
`local/` directory and is consumed through the same generic CLI any program uses.
See [docs/status.md](docs/status.md) for what works today and what does not.

## Building

Requirements: a Rust toolchain (1.88 or newer, edition 2024), CMake, a C compiler and
libclang (Unicorn is built from source by `unicorn-engine-sys`, whose bindings are
generated with bindgen). On Linux, ALSA headers are needed for host audio. On
Debian/Ubuntu:

```bash
sudo apt-get install cmake clang libclang-dev pkg-config libasound2-dev
```

```bash
cargo build --release
```

The binary is `target/release/drydock`.

## Running a program

Point `--root` at the installation directory and `--exe` at the executable inside it.
The installation is mounted read-only as `C:\GAME` (change with `--mount`); writes go
to an in-memory overlay, which `--overlay <dir>` persists across runs. An `--exe`
outside `--root` (such as an unpacked copy of the executable) is mounted beside the
installation under its file name.

Interactive play (desktop window, realtime pacing, host audio and input):

```bash
drydock run --mode play --exe Midtown2.exe --root /path/to/installation --overlay local/overlay
```

Headless, scripted, no window and no audio device:

```bash
drydock run --mode headless --exe Midtown2.exe --root /path/to/installation --overlay local/overlay --audio discard --scenario local/scenarios/drive.json --artifacts local/artifacts/drive
```

Each run writes `report.json` (stop reason, virtual and wall time, speed, API counts,
diagnostics, threads, audio voices), `provenance.json` (module hashes, profiles,
renderer, the recorded input timeline, debugger mutations) and `final.png` to the
artifacts directory.

Other useful options (see `drydock run --help`):

| Option | Purpose |
| --- | --- |
| `--seconds N` | Run for N seconds of virtual time (headless without a scenario). |
| `--pacing realtime\|unthrottled` | Headless pacing (default unthrottled). |
| `--audio device\|wav:<file>\|discard\|none` | Audio output; parts can be combined with `+`. Defaults to `device` in play mode and `none` headless. |
| `--overlay <dir>` | Persist guest file and registry changes in this directory. |
| `--reg <file.reg>` | Import registry entries before start. |
| `--map 'GUEST=HOST'` | Mount a host file or directory read-only at a guest path, such as fonts at `C:\WINNT\Fonts`. |
| `--restore <file.ddck>` | Start from a disk checkpoint instead of process start. |
| `--replay-input <file>` | Replay an input timeline (or a run's `provenance.json`). |
| `--trace <API>` | Record calls of matching APIs (substring match). |
| `--cpu-mhz N`, `--cpu-mips N` | Guest CPU profile (default 800 MHz, 800 MIPS). |
| `--software-gpu`, `--gpu-backend <b>` | Choose the wgpu adapter or backend. |
| `--joystick` | Attach a joystick (a SideWinder Force Feedback 2 with force feedback); in play mode a host game controller drives it. |
| `--joystick-device <sel>` | Host controller for the joystick: `045e:001b` (vendor:product) or part of its name; `none` for scripted input only. |
| `--force-feedback-strength N` | Scale force feedback on the host controller to N percent (default 100). |
| `--no-force-feedback` | Do not play force feedback on the host controller. |
| `--force-feedback-log <file>` | Write the joystick's force-feedback commands (JSON lines with virtual time). |

`drydock inspect --exe <file>` prints PE headers, sections, imports and hashes.

## Joysticks and force feedback

With `--joystick`, play mode uses the first game controller it finds (or one plugged
in later) and plays the program's force-feedback effects on it when it is a USB PID
force-feedback device, such as the Microsoft SideWinder Force Feedback 2. No driver
is needed: Drydock talks to the controller through HID. Check a controller first:

```bash
drydock joystick --list
drydock joystick --test-force-feedback --seconds 20
```

The second command prints the controls found, plays a few test effects (hold the
stick loosely: the SideWinder only applies force with a hand on the grip), reports
how many of them the stick confirmed playing, then shows live input. Controllers
report only changes, and the SideWinder cannot be asked for its position, so move the
throttle once after it connects. `--save-descriptor <file>` writes the
controller's HID report descriptor, which is what to attach to a bug report. On
Linux, the user needs access to the controller's `/dev/hidraw*` node (a udev rule).

## Agent protocol

`drydock serve --stdio` accepts one JSON request per line and answers each with a
correlated JSON response: open a session, run bounded intervals, inject input,
capture frames, read memory and registers, set breakpoints and watchpoints, trace
APIs, and create or restore savepoints. See [docs/protocol.md](docs/protocol.md).
Declarative scenarios are described in [docs/scenarios.md](docs/scenarios.md).

## Headless rendering without a GPU (Linux)

Rendering uses wgpu offscreen targets and never needs a display server. On a Linux
machine without a GPU, install Mesa's software Vulkan driver (Lavapipe), e.g.
`apt install mesa-vulkan-drivers` on Debian/Ubuntu, and select it:

```bash
drydock run --mode headless --software-gpu --gpu-backend vulkan --exe app.exe --root /path/to/app --audio discard --seconds 30
```

`--software-gpu` requests wgpu's fallback adapter; if none exists the run stops with
an error naming the missing adapter rather than continuing without rendering. The
adapter actually used is recorded in `provenance.json`. This configuration has been
tested in a Debian aarch64 container (`mesa-vulkan-drivers`); software rendering is
slower than a GPU.

## Tested platforms

| Host | Status |
| --- | --- |
| macOS 26 on Apple Silicon (M5 Max), Metal | Developed and tested: interactive and headless |
| Linux aarch64 (Debian container), Lavapipe | Tested headless: all tests pass, retail workload runs with identical guest execution |
| Linux x86-64 | Expected to work like aarch64; untested |
| Windows | Untested |

## Tests

```bash
cargo test --release --workspace
```

Tests never use commercial files. Guest programs for tests are generated from source
by the `drydock-fixtures` crate (a small PE32 builder plus iced-x86's assembler), so
`cargo test` is all that is needed; see [docs/architecture.md](docs/architecture.md#tests).
GPU tests skip with a message when no adapter is available.

## Documentation

- [docs/architecture.md](docs/architecture.md): execution, ABI, time, objects, devices, checkpoints
- [docs/protocol.md](docs/protocol.md): the JSON control protocol
- [docs/scenarios.md](docs/scenarios.md): declarative scenarios
- [docs/status.md](docs/status.md): implemented semantics, known gaps, tested paths, measurements
- [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md): dependencies, licences, bundled fonts

## Licence

Drydock links the Unicorn engine, which is licensed GPL-2.0-only; Drydock is
therefore distributed under the GNU General Public License version 2 only (see
[LICENSE](LICENSE)). Unicorn 2.1.5 is vendored under `vendor/` with one documented
performance patch ([vendor/README.md](vendor/README.md)). Some transitive dependencies are Apache-2.0-only, which raises a
compatibility question for binary distribution; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
