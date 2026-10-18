# Third-party notices

Drydock's own source is licensed GPL-2.0-only (see `LICENSE`). This file lists the
components it builds on, their licences, and an open licensing question. The full
dependency list with exact versions is `Cargo.lock`; licence metadata for every crate
can be listed with `cargo metadata`.

## Bundled in this repository

**Liberation fonts 2.x** (`assets/fonts/`: Liberation Sans Regular and Bold,
Liberation Serif Regular, Liberation Mono Regular). Digitized data copyright (c)
2010 Google Corporation with Reserved Font Arimo, Tinos and Cousine; copyright (c)
2012 Red Hat, Inc. with Reserved Font Name Liberation. Licensed under the SIL Open
Font License 1.1, reproduced in `assets/fonts/LICENSE`. The fonts are used to
rasterize GDI text for guest programs; they are unmodified.

## Derived from FFmpeg

**Indeo 5 decoder** (`crates/drydock-media/src/indeo5.rs`, `ivi_dsp.rs`,
`ivi_tables.rs`). A port of FFmpeg n7.1's `libavcodec/indeo5.c`, `ivi.c`,
`ivi_dsp.c` and `indeo5data.h` to Rust, restricted to the Indeo 5 paths. Copyright
(c) 2009-2011 Maxim Poliakovski. FFmpeg licenses this code under the GNU Lesser
General Public License version 2.1 or later; section 3 of that licence allows a
copy to be distributed under the GNU General Public License instead, and Drydock
does so, under GPL-2.0, keeping the original copyright notices in each file. The
tables were converted mechanically from the C sources.

## Principal dependencies

| Component | Version | Licence | Use |
| --- | --- | --- | --- |
| Unicorn engine (`unicorn-engine`, `unicorn-engine-sys`, including the bundled QEMU-derived C sources) | 2.1.5 (pinned; `unicorn-engine-sys` vendored in `vendor/` with the modifications listed in `vendor/README.md`) | GPL-2.0 | x86 CPU emulation |
| wgpu, wgpu-core, wgpu-hal, wgpu-types | 29.0.4 (pinned) | MIT OR Apache-2.0 | Graphics |
| naga (via wgpu) | 29.x | MIT OR Apache-2.0 | Shader translation |
| iced-x86 | 1.21.0 (pinned) | MIT | Instruction decoding (counting), test assembler |
| winit | 0.30 | Apache-2.0 | Desktop window and input |
| cpal | 0.16 | Apache-2.0 | Host audio output |
| fontdue | 0.9 | MIT OR Apache-2.0 OR Zlib | Font rasterization |
| ttf-parser | 0.25 | MIT OR Apache-2.0 | Names and metrics of guest TrueType fonts |
| png | 0.17 | MIT OR Apache-2.0 | PNG encoding |
| serde, serde_json | 1.x | MIT OR Apache-2.0 | Serialization |
| rmp-serde | 1.3 | MIT | Checkpoint encoding |
| flate2 (miniz_oxide backend) | 1.x | MIT OR Apache-2.0 | Checkpoint compression |
| clap | 4.x | MIT OR Apache-2.0 | Command line |
| sha2, hex | 0.10, 0.4 | MIT OR Apache-2.0 | Provenance hashes |
| libc | 0.2 | MIT OR Apache-2.0 | Host process/thread calls |

The remaining transitive dependencies (about 350 crates in total) are licensed under
MIT, Apache-2.0, BSD, Zlib, ISC, Unlicense, CC0-1.0 or combinations of these; a few
are offered under an LGPL-2.1-or-later alternative among permissive ones.

## Open question: Apache-2.0-only components

Unicorn declares GPL-2.0 (taken here as GPL-2.0-only; nothing in its metadata grants
"or later"). A small number of components in the build are licensed Apache-2.0
only, without a GPL-compatible alternative:

- `winit` and `dpi` (desktop window; `dpi` is "Apache-2.0 AND MIT"),
- `cpal` (host audio),
- `codespan-reporting` and `spirv` (used by wgpu's shader compiler naga),
- on Linux: `sctk-adwaita`'s `ab_glyph`, `ab_glyph_rasterizer`, `owned_ttf_parser`,
  and `x11rb`'s `gethostname`; on Windows: `glutin_wgl_sys`.

The Free Software Foundation considers the Apache License 2.0 incompatible with
GPLv2. Building and running Drydock locally does not distribute anything, but
distributing a binary that combines these components with Unicorn may not be
permitted. No binaries are published by this project. Resolving this (for example
by separating the GPL-licensed emulation into a process that does not link the
Apache-2.0-only crates, or by replacing those crates) is listed as an open item in
`docs/status.md`. No claim is made here that the combined work may be distributed.

## Reference material

BottleShip and Theseus (Apache-2.0) and retrowin32 are related projects in this
space. No source code from them is included in Drydock. Windows and DirectX
behaviour is implemented from public documentation and observed program behaviour.
