# Control protocol

`drydock serve --stdio` exchanges line-delimited JSON on stdin/stdout. Diagnostic
logging goes to stderr only, so stdout carries nothing but protocol messages.

## Messages

Request (one per line):

```json
{"id": 7, "method": "run", "params": {"seconds": 2.5}}
```

Response, echoing the request id:

```json
{"id": 7, "result": {"stop": "TimeReached", "virtual_s": 12.5, "advanced_s": 2.5, "wall_s": 0.9, "frames_presented": 301, "insns": 1834229113}}
{"id": 8, "error": {"code": "failed", "message": "no session is open (call \"open\" first)"}}
```

Requests are handled in order and each gets exactly one response. The exception is
`cancel`, which is answered immediately (out of order) and interrupts a `run`,
`step` or `scenario` in progress; that request then returns with stop `Cancelled`.
Error codes: `parse` (the line is not JSON; `id` is null), `failed` (the request
could not be carried out; the session is unchanged unless the message says
otherwise), `internal` (a runtime bug; the session is closed).

Large outputs are written to files and returned as paths (`screenshot` with `path`,
`checkpoint.save`); traces are bounded rings.

## Values

- Addresses and other integers may be given as JSON numbers or strings (`"0x401000"`).
- Byte strings are lowercase hex (`"hex": "b82a000000c3"`).
- Times are in seconds of virtual time unless named `wall_s`.
- `stop` values: `"TimeReached"`, `"InsnLimit"`, `{"Idle": {"next_event": ns|null}}`,
  `{"Breakpoint": {"tid", "addr"}}`, `{"Watchpoint": {"tid", "addr", "write", "eip"}}`,
  `{"ApiBreak": {"tid", "api"}}`, `{"Presented": {"frame"}}`, `{"Exited": {"code"}}`,
  `"Halted"`, `"Cancelled"`.

## Methods

### Lifecycle

| Method | Params | Result |
| --- | --- | --- |
| `hello` | – | `protocol` (1), `drydock_version`, `methods` |
| `open` | the `drydock run` options as JSON (see below) | `exe`, `image_base`, `virtual_s`, `artifacts` |
| `close` | – | `{}` |
| `shutdown` | – | `{}`, then the server exits |

`open` accepts the same options as `drydock run`, with option names as keys
(`-` or `_`): `{"exe": "app.exe", "root": "/path/to/app", "overlay": "local/ov",
"audio": "discard", "reg": ["a.reg"], "joystick": true, "restore": "x.ddck"}`.
Booleans set flags; arrays repeat an option. Pacing defaults to `unthrottled`.
One session is open at a time; `open` replaces it.

### Execution

| Method | Params | Result |
| --- | --- | --- |
| `run` | `seconds`, `frames` (stop after N more presentations), `max_insns`, `until_present` (bool); at least one | `stop`, `virtual_s`, `advanced_s`, `wall_s`, `frames_presented`, `insns` |
| `cancel` | – | `cancelled` (whether a session was open) |
| `step` | `step`: one scenario step (see scenarios.md) | `outcome` (`"ok"`, `{"failed": reason}` or `{"stopped": stop}`), `log`, `virtual_s` |
| `scenario` | `scenario` (inline) or `path` | `ok`, `stop`, `log` |

Running continues through loading screens and blocked waits: virtual time advances
whether or not frames are presented. Pause is simply not issuing `run`; the session
holds its state between requests.

### Input

| Method | Params | Result |
| --- | --- | --- |
| `input` | `events`: list of input events, each optionally `{"at": seconds_from_now, "event": {...}}` | `scheduled` |
| `input.recording` | – | `events`: every input event applied so far, with virtual timestamps |

Input events (the same JSON appears in recordings and `provenance.json`):

```json
{"type": "key", "vk": 13, "scan": 28, "extended": false, "down": true}
{"type": "mouse_move", "x": 320, "y": 240}
{"type": "mouse_rel", "dx": 5, "dy": -2}
{"type": "mouse_button", "button": 0, "down": true}
{"type": "wheel", "delta": 120}
{"type": "joy_axes", "x": 32768, "y": 0, "z": 32768, "r": 32768}
{"type": "joy_button", "button": 0, "down": true}
{"type": "joy_pov", "pov": 9000}
{"type": "focus", "active": true}
```

Joystick axes are 0–65535 (`z` is the throttle, `r` the twist); the hat (`pov`) is in
hundredths of a degree clockwise from north, 4294967295 when centred.

Scenario steps (`key`, `click`, `type`, …) are usually more convenient; send them
with `step`. A recording can be replayed in a later session with the `replay_input`
open option.

### Inspection

| Method | Params | Result |
| --- | --- | --- |
| `state` | – | `virtual_s`, `insns`, `frames_presented`, `display_mode`, `exited`, `halt`, `windows`, `threads` (state including any wait, EIP, ESP), `modules` (name, base, size, hash), `audio` (playing voices), `missing`, `dialogs_open` |
| `events` | `since_s` | diagnostic `events` at or after that time, `debug_output` |
| `report` | – | the run report (as in `report.json`) plus `provenance` |
| `screenshot` | `path` (optional) | `width`, `height`, and `path` or `png_base64` |
| `memory.read` | `addr`, `len` (≤ 1 MiB) | `addr`, `hex` |
| `memory.search` | `hex` or `text`, `limit` | `hits` (addresses) |
| `surfaces` | – | `surfaces` (id, size, pixel format, guest address and pitch, row order, caps, and the DirectDraw objects showing them with their reference counts and interface pointers) and the scanned-out `front` surface |
| `regs.get` | `tid` (default: current thread) | `tid`, `regs` (general, EIP, EFLAGS, FS base, x87, MXCSR, XMM) |

### Debugging

| Method | Params | Result |
| --- | --- | --- |
| `breakpoint.add` / `breakpoint.remove` | `addr` | `{}` / `removed` |
| `watchpoint.add` | `addr`, `len`, `write` (default true), `read` | `{}` |
| `api_break.add` / `api_break.remove` | `name` (`"KERNEL32!CreateFileA"` or `"CreateFileA"`) | `{}` |
| `trace.set` | `api`: list of substrings (empty disables), `cap`, `blocks` (block-trace ring size, 0 = off) | `{}` |
| `trace.get` | – | `api` (calls with arguments, caller, thread, time), `blocks` |
| `memory.write` | `addr`, `hex` | `written` |
| `regs.set` | `tid`, `regs`: subset of the `regs.get` fields | `{}` |

`memory.write` and `regs.set` are debugger mutations: each is logged with its virtual
time and listed in the report's provenance, so modified sessions can be excluded from
claims about unmodified programs.

### State

| Method | Params | Result |
| --- | --- | --- |
| `savepoint.create` | `name` | checkpoint info (`label`, `virtual_s`, `insns`, `frames_presented`, `exe_sha256`, …) |
| `savepoint.restore` | `name` | checkpoint info |
| `savepoint.list` | – | `savepoints` |
| `savepoint.delete` | `name` | `deleted` |
| `checkpoint.save` | `path`, `label` | `path`, `bytes`, `info` |
| `checkpoint.load` | `path` | checkpoint info |

Savepoints live in the server process; checkpoints are files that a fresh process
can load (`checkpoint.load`, or `open` with `restore`). Restoring a savepoint and
continuing creates a branch; the original savepoint stays available. Loading a
checkpoint taken with a different executable is refused.

## Example

```text
> {"id":1,"method":"open","params":{"exe":"app.exe","root":"/data/app","audio":"discard"}}
< {"id":1,"result":{"artifacts":"local/artifacts/serve","exe":"C:\\GAME\\app.exe","image_base":4194304,"virtual_s":0.0}}
> {"id":2,"method":"run","params":{"seconds":5}}
< {"id":2,"result":{"advanced_s":5.0,"frames_presented":210,"insns":812118310,"stop":"TimeReached","virtual_s":5.0,"wall_s":2.1}}
> {"id":3,"method":"step","params":{"step":{"key":"ENTER"}}}
< {"id":3,"result":{"log":[],"outcome":"ok","virtual_s":5.13}}
> {"id":4,"method":"savepoint.create","params":{"name":"after-enter"}}
> {"id":5,"method":"screenshot","params":{"path":"/tmp/frame.png"}}
> {"id":6,"method":"shutdown"}
```

The integration test `crates/drydock/tests/serve.rs` drives a complete session this
way against a generated fixture program.
