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

To update Unicorn: replace `unicorn-engine-sys/` with the new published crate, apply
the patches in order (`patch -p1 -d vendor < vendor/patches/0001-…`), and re-run the
tests.
