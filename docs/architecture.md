# Drydock architecture

Drydock emulates one 32-bit Windows user-mode process. Guest x86 code runs in
Unicorn; everything the guest calls into — system DLLs, COM objects, devices — is
Rust code operating on explicitly owned session state. This document describes how
the pieces fit and which invariants they keep.

## Crates

| Crate | Responsibility |
| --- | --- |
| `drydock-core` | Guest address space, PE parsing, the CPU wrapper around Unicorn, the virtual clock |
| `drydock-win32` | The session: loader, scheduler, kernel objects, system DLLs, COM, devices, diagnostics, checkpoints |
| `drydock-gfx` | wgpu renderer: textures, fixed-function shader, draws, readback (headless capable) |
| `drydock-audio` | Host audio sinks: device (cpal), WAV file, discard, tee |
| `drydock-media` | AVI container parsing and the Indeo 5 video decoder (ported from FFmpeg) |
| `drydock` | CLI, pacing driver, scenario runner, JSON protocol server, desktop player |
| `drydock-fixtures` | Generates PE32 test programs from Rust source, and Indeo 5 / AVI test media (tests only) |

## Session and state ownership

`drydock_win32::session::Session` owns all guest-visible state: memory, threads,
kernel objects, modules, windows, surfaces, COM objects, device state, the virtual
clock and the input queue. Every object has a stable integer id (handles, COM object
ids, surface ids, thread ids); no host pointer is ever exposed to the guest, and no
state is process-global. Host bindings that are not guest state — the GPU renderer,
the audio sink, the cancellation flag, API breakpoints — live in `Session::host` and
are never checkpointed.

The CLI, protocol server and desktop player all create a session through
`launch::build_session` and drive it through `Driver` (pacing) or `Session::run`
directly. The player renders the same frames the headless runner captures; opening
a window or an audio device only consumes output that exists anyway.

## Guest memory

`AddressSpace` (`drydock-core/src/mem.rs`) keeps regions with per-page state
(reserved/committed) and Win32 protections. Each region is backed by a host buffer
mapped into Unicorn with `mem_map_ptr`, so Rust reads and writes guest memory
directly after a bounds and permission check; guest pointers are never dereferenced
as host pointers. Mapping and protection changes are queued and applied to Unicorn
before the next slice (`Cpu::flush_mem_ops`).

Writes from Rust (API results, blits, readbacks, debugger writes) queue a code
invalidation for their range. The CPU tracks which pages have ever held translated
code and only invalidates translations there, so ordinary API writes cost nothing.
Guest self-modifying code is handled by Unicorn itself.

Protections follow Windows 2000 on x86 without DEP: every readable page is
executable for the guest. In the JIT, only `PAGE_EXECUTE*` pages are mapped
executable up front; a readable page gains execute permission on its first
instruction fetch (`AddressSpace::grant_exec`), and a second fetch fault on the same
page is a real fault. Unicorn is used with a small patch (`vendor/README.md`) so that
stores to memory mapped non-executable stay on its fast path; stores to executable
memory keep the code-invalidation checks that make self-modifying code work. No
Unicorn memory hooks are installed in normal operation (any installed memory hook
forces Unicorn's slow path); faults are reported by `emu_start` and their address is
reconstructed by decoding the faulting instruction's memory operands with iced-x86.

Heaps (`heap.rs`) keep their bookkeeping in Rust, outside guest memory, so a guest
overrun cannot corrupt allocator metadata; allocation is deterministic best fit.

## Loading

`loader.rs` maps PE32 images (sections, zero fill, relocation when the preferred base
is taken, imports by name and ordinal, exports and forwarders, resources, TLS
templates and callbacks) and runs DLL entry points in dependency order before the
process entry point. Guest DLLs are found along the Windows search order (exe
directory, system directory, Windows directory, current directory); `LoadLibrary`
and `GetProcAddress` work at run time.

System DLLs (`KERNEL32`, `USER32`, `GDI32`, `DDRAW`, `DSOUND`, …) are always the
Rust-provided builtins. Each builtin is a real PE image in guest memory with an
export table whose entries point at 4-byte thunks (`int 0x2e; ret`). Imports bind to
these thunks like any other export. An import a builtin does not implement binds to
a dynamically allocated stub that halts the session with an *unimplemented*
diagnostic when called. COM vtable entries and system window procedures are internal
thunks in the same images.

## Execution and the API boundary

`Cpu::run_slice` runs guest code from the current EIP until one of:

- an `int 0x2e` trap (an API call), raised by Unicorn's interrupt hook,
- the instruction budget is exhausted (checked by a block hook at block boundaries),
- a breakpoint or watchpoint,
- a fault (unmapped/protected access, invalid instruction, CPU exception).

A block hook (a plain C callback) counts retired instructions per executed block;
the vendored Unicorn passes each block's instruction count with the hook arguments,
so nothing is decoded at run time and no per-instruction callback runs during normal
execution. A stop requested by the hook takes effect before the block runs, so budget
stops land exactly on block boundaries. Full block tracing is an opt-in bounded ring.

On a trap the session dispatches the thunk's handler with the guest stack in place.
Handlers read arguments from `[esp+4..]`, write results to guest memory, and return
one of: a value (stdcall/cdecl cleanup is applied from the export's convention),
a 64-bit or x87 floating-point result, a *guest call* (callback), a *block*, or an
exit. Last-error values are kept per thread in the TEB.

### Callbacks and continuations

A handler that must call guest code (window procedures, enumeration callbacks, COM
callbacks, timer procedures) returns a `GuestCall` plus a continuation (`Cont`). The
session pushes a frame, sets up the guest stack so the callback returns into the
`CallbackReturn` thunk, and resumes the handler's continuation with the callback's
result. Continuations are plain serializable enums, not host closures, so a thread in
the middle of a callback can be inspected and checkpointed.

### Blocking and threads

A handler that must wait (Sleep, WaitFor*, GetMessage, critical sections, dialogs,
multimedia timers) returns `Block { wait, deadline, cont }`. The thread is parked in
the scheduler with that record; `wake_threads` re-evaluates waits as objects change
and resumes the continuation with the outcome.

The scheduler (`sched.rs`) runs guest threads one at a time on the single Unicorn CPU
with a 2,000,000-instruction quantum, saving and restoring the full register file
(general, segment, x87, SSE) on switches. FS points at each thread's TEB through a
per-thread GDT descriptor.

### Exceptions

Faults and `RaiseException` are delivered through the guest's SEH chain with Windows'
two-phase semantics (`seh.rs`): handlers are called as guest callbacks, unwinding
runs `RtlUnwind`, and an unhandled exception reaches `UnhandledExceptionFilter`. A
fault is never swallowed to keep a broken process running; if nothing handles it the
session halts with a report.

## Virtual time

One virtual clock (`drydock-core/src/clock.rs`) counts nanoseconds since session
start. It advances only by:

1. retired guest instructions, converted with the CPU profile's rational
   nanoseconds-per-instruction (default *pentium3-800*: 1.25 ns per instruction,
   i.e. 800 million instructions per second, TSC at 800 MHz);
2. a fixed 200 ns charge per API call;
3. jumps to the next scheduled event when every thread is blocked.

All guest-visible time derives from it: `GetTickCount`/`timeGetTime` (with a 600 s
uptime at session start), `QueryPerformanceCounter` (3,579,545 Hz), `RDTSC` (TSC rate
times virtual time, computed inside the slice from the instruction count), sleeps and
wait timeouts (rounded to the timer resolution set by `timeBeginPeriod`), multimedia
and window timers, input timestamps, vertical blank (60 Hz), and DirectSound play
cursors. Each slice's instruction budget is cut at the next scheduled event so events
fire at their time, not late.

Instruction counting is not cycle accuracy: every instruction costs the same, and
caches and pipelines are not modelled. A `rep`-prefixed string instruction counts
once per iteration plus once when its count is exhausted (Unicorn re-enters its block
each iteration; `rep movsb` with ECX=100 counts 101). The model is deterministic:
the same inputs at the same virtual times produce the same execution on the same
build and host.

Host wall time is used only for pacing, measurement and watchdogs (`driver.rs`):
*realtime* sleeps when virtual time is ahead of wall time and re-anchors after falling
behind by more than 100 ms; *unthrottled* runs as fast as possible and jumps over idle
periods; protocol `run` requests advance by a requested interval, instruction budget,
or number of presented frames. The pacing mode never changes guest-visible behaviour,
only how long the host waits.

## Graphics

Surfaces (`surface.rs`) live in guest memory (system memory or a 32 MB VRAM heap),
with format, pitch, palette and colour keys. Each surface has CPU-dirty and GPU-dirty
flags: CPU writes (locks, blits, GDI) mark it CPU-dirty and it is uploaded before the
GPU next reads it; GPU rendering marks it GPU-dirty and it is read back into guest
memory before the CPU next reads it (locks, blits, GetDC, captures). Flips swap the
surface ids behind the flip chain's objects and retire at the next vertical blank.

Direct3D 7 state is translated into an uber-shader in `drydock-gfx/src/ff.wgsl`:
transforms, up to 8 lights, materials, two texture stages (colour/alpha ops and
arguments), texture coordinate generation and transforms, alpha test, fog, colour
keying, depth test/write/bias, culling and blending. D3D's pixel-centre convention
and screen-space (pre-transformed) vertices are handled explicitly. Draws accumulate
into a pass that is submitted at presentation or when a target is read back; clears
are draws so they obey the same ordering. Depth buffers are `Depth32Float`; guest
writes to depth surfaces are uploaded per pixel.

Headless runs use the same code with offscreen targets. Adapter selection, backend,
and errors are reported; a failed GPU initialisation or a wgpu validation error
halts the session instead of silently discarding draws.

## Audio

DirectSound (`com/dsound.rs`) models each buffer as a timeline: play position is a
function of virtual time, frequency and the last explicit anchor (Play, Stop,
SetCurrentPosition, SetFrequency). Cursors, looping, end of playback and position
notifications therefore progress on guest time whether or not anything is listening.
When an audio sink is attached, a software mixer renders all playing buffers (volume,
pan, frequency, 3D distance attenuation, panning and Doppler) at 44.1 kHz in 5 ms
virtual-time ticks and pushes the samples to the sink: the host device (cpal), a WAV
file, or a discarding sink. The host queue never feeds back into guest time.

The mixer timeline starts with the first sound source (DirectSound, or a movie's
sound track) and then keeps running, so a WAV capture stays aligned with virtual
time.

DirectMusic provides the performance, port, loader and composer objects; segment
playback and DLS synthesis are not implemented (see `status.md`). MCI answers CD
audio requests with "device not ready" because no CD-ROM drive can be mapped yet.

## Movies

`MSVFW32.DLL` provides the MCIWnd window (`dll/msvfw32.rs`). Its window procedure
opens the AVI file through the VFS, sizes the window to the movie and answers the MCI
commands sent as messages. A playing movie's position is a function of virtual time,
like a DirectSound buffer: the next frame time is a scheduler event, so playback
continues while the guest thread sleeps between polls, as it does on Windows where an
MCI thread plays the file. Due frames are decoded with `drydock-media`'s Indeo 5
decoder and drawn into the window's client area on the scanned-out surface (clipped
to its ancestors), followed by a presentation; the PCM sound track is rendered by the
DirectSound mixer. Decoded pictures are a host-side cache: a checkpoint stores the
playback position, and after a restore the decoder decodes from the first frame
again, which yields the same pictures.

## Input

Window messages, `GetAsyncKeyState`/`GetKeyState`, and DirectInput devices (buffered
and polled keyboard, mouse, joystick) all read one `InputState`. Host input (the
player), scenarios and the protocol schedule timestamped events into it; events apply
when virtual time reaches them. Every applied event is recorded, which is the input
timeline written to `provenance.json` and accepted by `--replay-input`. Focus and
key-release events are explicit so a cancelled experiment cannot leave keys stuck.

## Diagnostics

Missing emulator functionality is reported as structured events
(`Session::missing`), distinct from guest-visible API errors. Calling an
unimplemented API or method halts the session with the operation, decoded
arguments, guest EIP, thread and the recent API ring. Reports also contain API call
counts, thread states, display mode, audio voices and where translated code lives.

## Checkpoints

`Session::checkpoint` captures, between slices, everything guest-visible: all memory
regions with page states and protections, every thread's full register file and
scheduler state (including pending continuations and waits), the clock, kernel
objects, modules, heaps, the file-system overlay, the registry, windows and
messages, GDI objects, surfaces, COM objects with reference counts, DirectSound
buffers and their timelines, DirectMusic and MCI state, input, and diagnostics.
GPU-only surface contents are read back first, so guest memory is authoritative.

Restoring replaces that state, re-binds thunk handlers by module and name, applies
the CPU's rates and counters, flushes all translated code, and drops renderer
textures so they are rebuilt from guest memory. Unicorn contexts are never serialized.

In-memory savepoints are clones of the `Checkpoint` value. Disk checkpoints are a
magic header, a JSON summary (format version, label, virtual time, executable hash)
and the state as deflated MessagePack with struct fields stored by name, so state
added later with a default value can still be read. A checkpoint is only accepted by the same
format version and the same executable hash; host mounts are kept from the session
restoring it, so a checkpoint can be restored against another copy of the
installation.

Every point between `Session::run` calls is a safe boundary: there is no in-flight
host work that the format cannot represent. Reproducibility is guaranteed for the
same build, backend and host configuration; cross-version or cross-platform
equivalence has not been established.

## Tests

`cargo test --workspace` runs:

- `drydock-core/tests/cpu.rs`: register round trips including x87 state, budgets,
  breakpoints mid-block, faults, FS/TEB, RDTSC against virtual time, translated-code
  invalidation after Rust-side writes and guest self-modifying code.
- `drydock-gfx/tests/headless.rs`: exact pixel coverage of pre-transformed quads,
  depth testing, texture modulation with alpha test, cull-mode winding, depth upload.
- Unit tests in `drydock-win32`: heap coalescing, VFS overlay semantics, registry
  import, DXT decoding, FVF layouts, DirectSound cursor and mixer behaviour, executing
  rewritten code from data pages, checkpoint restore (in memory and through a file
  into a fresh session) reproducing the same execution.
- `drydock/tests/serve.rs`: the stdio protocol against a fixture program, including
  savepoints, memory writes, register reads and process exit.

Fixture programs are built by `drydock-fixtures`: `PeBuilder` lays out an import
table, a data section and a code section assembled with iced-x86's `CodeAssembler`
at a fixed base. No binary fixtures are checked in.
