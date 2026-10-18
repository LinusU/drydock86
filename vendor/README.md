# Vendored dependencies

## unicorn-engine-sys 2.1.5

`unicorn-engine-sys/` is the `unicorn-engine-sys` 2.1.5 crate as published on
crates.io (Unicorn 2.1.x C sources, including the QEMU-derived TCG code), licensed
GPL-2.0 (see `unicorn-engine-sys/COPYING` and the notices in its sources). It is used
through `[patch.crates-io]` in the workspace `Cargo.toml`, so the version stays pinned
at 2.1.5 while carrying the modifications listed below. Nothing else in the crate has
been changed.

### Modifications

`patches/` holds each change as a unified diff against the published crate.

**0001: store fast path for non-executable memory** (2026-10-17,
`qemu/accel/tcg/cputlb.c`)

Unicorn stubs out QEMU's dirty-memory tracking: `cpu_physical_memory_is_clean()`
always returns true, so every TLB entry for a writable RAM page gets `TLB_NOTDIRTY`
and its stores take the slow path (`store_helper` → `notdirty_write`, with region
lookups). `notdirty_write` only clears the flag for TLB entries without execute
permission, but x86 without paging marks every TLB entry executable, so for 32-bit
x86 guests every store to any page stayed on the slow path permanently.

The patch decides with Unicorn's own region permissions instead: `TLB_NOTDIRTY` is
set only for regions mapped `UC_PROT_EXEC`, and `notdirty_write` clears it for regions
that are not. Translated code can only come from executable regions, so stores to
other memory need no code invalidation; stores to executable regions keep the
original behaviour, so self-modifying code is still detected. Drydock maps only
`PAGE_EXECUTE*` pages executable up front and grants execute permission to other
readable pages on their first instruction fetch (changing the mapping flushes the
TLB), which keeps this sound.

Effect on the Midtown Madness 2 race benchmark (`local/bench.sh`, 8.86 s of driving
from a checkpoint, Apple M5 Max): 0.53× → 0.96× realtime, with an identical guest
instruction count and bit-identical frames. All tests, including the self-modifying
code and translated-code invalidation tests, pass unchanged.

**0002: per-access exit check only with memory hooks** (2026-10-17,
`qemu/tcg/tcg-op.c`, `qemu/accel/tcg/cputlb.c`)

Unicorn emits a call to `helper_check_exit_request` after every guest load and
store, so that emulation stops right after an invalid access or after a memory hook
requested a stop. Loads with invalid addresses already leave the translated block
directly from the load helper; the patch does the same for stores and emits the
per-access check only while memory hooks are installed (they are the remaining
reason for it). Stops requested elsewhere are unaffected: block starts still check
for them, and code hooks keep their own check. A program adding a memory hook to code
that was translated earlier must flush the translation cache to get the check; Drydock
does so when it adds or removes a watchpoint.

Effect on the same benchmark: best of three 0.997× → 1.135× realtime, identical guest
execution; the full drive scenario runs at 1.31× realtime.

**0003: instruction count for block hooks** (2026-10-17,
`qemu/accel/tcg/translator.c`)

Block hooks receive the block's size in bytes. The patch puts the block's instruction
count (known to the translator) in the upper 16 bits of that argument, so Drydock's
instruction-counting hook no longer decodes blocks or keeps a cache of counts.
Blocks are smaller than 64 KiB and hold at most 512 instructions, so both fit.
Counts are identical to the previous decode-based ones on the acceptance workload.

**0004: inline block counting** (2026-10-18, `qemu/accel/tcg/translator.c`,
`qemu/accel/tcg/tcg-runtime.{c,h}`, `qemu/x86_64.h`, `include/uc_priv.h`, `uc.c`)

Drydock counted instructions in a block hook, a C callback per executed block. The
patch adds `uc_drydock_fast_count()`: given pointers to an instruction counter, a limit,
a block counter and a bitset of code pages, the translator emits the counting as TCG
operations at the start of each block (load, compare with the limit, add the block's
instruction count, store) and calls a helper that stops emulation when the limit is
reached, in which case the block is neither entered nor counted, as before. The pages
holding translated code are recorded at translation time instead of at each execution.
With a null counter pointer the block hook works as before (Drydock uses it to record a
block trace). Counts, frames and stops are identical to the hook-based version.

**0005: x87 host-double fast paths** (2026-10-18, `qemu/fpu/softfloat.c`,
`qemu/target/i386/fpu_helper.c`, `uc.c`)

With the x87 control word at 53-bit precision and round-to-nearest-even (what Windows
programs run with), `floatx80_add/sub/mul/div` and `floatx80_to_float32` compute with
host doubles when the operands are exactly representable as doubles with moderate
exponents (zeros included), and the result is a normal double; the result is then the
correctly rounded one, bit for bit. Everything else (denormals, infinities, NaNs,
64-bit mantissas, other precisions or rounding modes, out-of-range results) takes the
original softfloat path. The inexact flag is not raised on the fast paths; the x87
model never exposes it (only the invalid flag is read, by the integer conversions).
`FLD m32` converts normal singles directly. `uc_drydock_set_fast_fp(0)` turns the
double paths off; `crates/drydock-core/tests/x87.rs` runs randomised x87 programs both
ways and compares every result and status word.

**0006: inline exit check, no JIT write toggling per block** (2026-10-18,
`qemu/include/exec/gen-icount.h`, `qemu/accel/tcg/cpu-exec.c`)

Each block began with a helper call that tests the exit request. The patch tests the
request in translated code (a load and a branch, as upstream QEMU does) and calls the
helper only when it is set. On Apple Silicon, `cpu_tb_exec` switched the thread
between executable and writable around every run of translated code; it now stays
executable, because the code generator, block chaining and cache flushes unlock for
themselves and `restore_jit_state()` puts the thread back when `uc_emu_start` returns.

**0007: larger minimum software TLB** (2026-10-18, `qemu/include/exec/cpu-defs.h`)

Unicorn shrinks the direct-mapped TLB to the number of entries recently in use, with a
minimum of 64. For a working set of a few hundred pages that leaves hot pages that share
a slot (addresses a multiple of the TLB size apart) evicting each other on every access:
Crimson Skies in flight took 93 million slow-path loads in 15 s of guest time, almost all
for five pages. The minimum is now 4096 entries (128 KiB per MMU mode), which brings that
to about one million. Results are unchanged.

To update Unicorn: replace `unicorn-engine-sys/` with the new published crate, apply
the patches in order (`patch -p1 -d vendor < vendor/patches/0001-…`), and re-run the
tests.
