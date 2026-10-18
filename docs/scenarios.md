# Scenarios

A scenario is a JSON file of ordinary steps — inputs, bounded waits, captures,
assertions and savepoints — run against a session with `drydock run --scenario
<file>` or the protocol's `scenario` / `step` methods. The executor knows nothing
about any program: scenarios only use input any user could give and observations the
runtime exposes. Scenarios for a particular program are local data (keep them under
the ignored `local/` directory), not part of Drydock.

```json
{ "name": "open-settings", "steps": [
  { "wait_for": { "window_class": "MainWnd" }, "timeout": 20 },
  { "run": 2.0 },
  { "click": [320, 240] },
  { "key": "TAB" },
  { "key": "O", "mods": ["ALT"] },
  { "type": "Player 1\n" },
  { "screenshot": "settings.png" },
  { "assert": { "frames_at_least": 100 } },
  { "savepoint": "settings-open" },
  { "key_down": "UP" },
  { "run": 3.0 },
  { "key_up": "UP" },
  { "restore": "settings-open" }
] }
```

A scenario succeeds when every step completes. It fails when a `wait_for` times out,
an `assert` does not hold, or the program halts; reaching process exit counts as
success only on the last step. The run report records each step with its virtual
time (`t`) and host time (`wall`), plus screenshots and savepoint timings.

## Steps

All times are virtual seconds. Input is scheduled at the current virtual time and
delivered through the same devices as human input (window messages, async key state,
DirectInput).

| Step | Effect |
| --- | --- |
| `{"run": s}` | Advance virtual time by `s`. |
| `{"key": K, "mods": [M...], "hold": s}` | Press and release key `K` (held 0.06 s by default) with modifiers held around it. |
| `{"key_down": K}` / `{"key_up": K}` | Press or release a key and leave it in that state. |
| `{"type": "text"}` | Type letters, digits, space and newline (Enter), with Shift for capitals. |
| `{"click": [x, y], "button": "left"\|"right"\|"middle"}` | Move the pointer to screen coordinates and click. |
| `{"click_here": "left"\|"right"\|"middle"}` | Click without moving the pointer (for cursors that programs draw from relative mouse motion). |
| `{"mouse_move": [x, y]}` | Move the pointer (absolute screen coordinates). |
| `{"mouse_rel": [dx, dy]}` | Relative motion, as from a captured mouse. |
| `{"release_all": true}` | Release every pressed key and button. |
| `{"answer_dialog": id}` | Answer the open message box with button id (1 = OK, 2 = Cancel, 6 = Yes, 7 = No). |
| `{"screenshot": "name.png"}` | Capture the displayed frame into the artifacts directory. |
| `{"wait_for": C, "timeout": s}` | Advance in 10 ms steps until condition `C` holds (default timeout 10 s). |
| `{"assert": C}` | Fail unless condition `C` holds now. |
| `{"savepoint": "name"}` | Keep an in-memory checkpoint for this run. |
| `{"save": "name.ddck"}` | Write a disk checkpoint into the artifacts directory. |
| `{"restore": "name"}` | Restore a savepoint, or a checkpoint file relative to the artifacts directory. |
| `{"note": "text"}` | No effect; documents the scenario. |

Key names: letters and digits, `F1`–`F24`, `ENTER`, `ESC`, `TAB`, `SPACE`,
`BACKSPACE`, arrow keys (`UP`, `DOWN`, `LEFT`, `RIGHT`), `HOME`, `END`, `PAGEUP`,
`PAGEDOWN`, `INSERT`, `DELETE`, `SHIFT`/`LSHIFT`/`RSHIFT`, `CTRL`/`LCTRL`/`RCTRL`,
`ALT`/`LALT`/`RALT`, `CAPSLOCK`, `PAUSE`, or any virtual-key code as `0x..`.

## Conditions

A condition object holds when all of its fields hold.

| Field | Holds when |
| --- | --- |
| `window_title` | a visible top-level or child window's title contains the text |
| `window_class` | a window of this class exists (case-insensitive) |
| `dialog_open` | a message box or dialog is (or is not) open |
| `api_called` | the API (e.g. `"DDRAW!IDirectDrawSurface7::Flip"`) has been called at least once |
| `frames_at_least` | at least this many frames have been presented |
| `debug_output_contains` | `OutputDebugString` output contains the text |
| `module_loaded` | a module with this name is loaded |
| `exited` | the process has (or has not) exited |
| `display_mode` | the display mode is `[width, height, bpp]` |

## Recording and replay

Every input event a run applies is recorded with its virtual timestamp in
`provenance.json` (`input_timeline`). Replaying it reproduces the run when the
executable, configuration and build are the same:

```bash
drydock run --exe app.exe --root /data/app --replay-input local/artifacts/run1/provenance.json --seconds 30
```

This works for runs recorded by the interactive player too, so a human session can be
replayed headlessly.
