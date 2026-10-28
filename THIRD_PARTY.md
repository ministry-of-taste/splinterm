# Third-party work

## foot

Splinterm takes architectural inspiration from foot, the fast Wayland terminal
emulator by Daniel Eklöf and contributors.

- Source: <https://codeberg.org/dnkl/foot>
- License: MIT
- Initial port baseline: foot 1.27.0, commit
  `3c5b584b0eafa772eb4376fb6eaf6643399e190e`
- Architectural decision: [`docs/adr/0001-foot-rust-port.md`](docs/adr/0001-foot-rust-port.md)

Terminal representations, grid behavior, parser recognition, and command
semantics in `crates/splinterm-terminal/src/` are translated from Foot's
`terminal.h`, `grid.h`, `grid.c`, `vt.h`, `vt.c`, `terminal.c`, `commands.c`,
`csi.c`, `osc.c`, `dcs.c`, `slave.c`, `render.c`, and `reaper.c` at the
revision above. The affected modules
record source-level provenance in their module documentation.

`crates/splinterm/src/box_drawing.rs` is a narrow safe-Rust translation of
Foot's `box-drawing.c` at the same revision. It currently covers U+2500,
U+250C, U+2510, U+253C, and the U+2800–U+28FF Braille range used by the
renderer evidence and full-screen TUI path.

Foot is Copyright (c) 2019 Daniel Eklöf and is distributed under the MIT
License. Splinterm's MIT `LICENSE` preserves the applicable permission and
warranty terms for these translations.

## png

The bounded Kitty and iTerm2 static-image subset decodes PNG through the Rust
`png` crate.

- Source: <https://github.com/image-rs/image-png>
- Version: 0.18.1
- License: MIT OR Apache-2.0

## flate2

The bounded Kitty static-image subset decodes explicitly requested zlib streams
through `flate2`'s safe Rust backend.

- Source: <https://github.com/rust-lang/flate2-rs>
- Version: 1.1.9
- License: MIT OR Apache-2.0
- Native zlib dependency: none; the workspace uses the Rust backend selected by
  the resolved feature graph

## rmcp

The optional `splinterm-mcp` split package uses the official Rust Model Context
Protocol SDK for its bounded local stdio server.

- Source: <https://github.com/modelcontextprotocol/rust-sdk>
- Version: exactly 2.2.0 (`rmcp` and `rmcp-macros`)
- License: Apache-2.0
- Default features: disabled
- Enabled rmcp features: `macros`, `schemars`, `server`, `transport-io`
- Protocol accepted by the shipped adapter: exactly MCP `2025-11-25`

The selected production feature tree contains no HTTP, SSE, OAuth, JWT, client,
child-process, tower, or elicitation dependency. The locked dependency and
license inventory and package validation retain that boundary. The workspace
MSRV is Rust 1.88. The Rust crate has `publish = false`; its executable is
distributed only through the explicit `splinterm-mcp` split package.

Slice 9 interoperability uses ephemeral `npx` executions of MCP Inspector 1.0.0
and the official conformance runner 0.1.16. Both are test/documentation tooling,
not package dependencies. Inspector is MIT licensed. The conformance framework
is MIT licensed; its server runner currently accepts URL transports only, so it
cannot execute Splinterm's stdio-only profile and no HTTP transport is added.

## seccompiler

The daemon's sealed offline handoff preflight uses `seccompiler` to install an
inherited seccomp-BPF rule that denies process and thread creation before an
untrusted candidate image executes.

- Source: <https://github.com/rust-vmm/seccompiler>
- Version: 0.5.0
- License: Apache-2.0 OR BSD-3-Clause
- Filter policy: allow all other syscalls; return `EPERM` for `fork`, `vfork`,
  `clone`, and `clone3`

## rustix

Splinterm uses `rustix` for safe Linux PTY, termios, process, signal, and
owned-file-descriptor operations in `splinterm-pty`.

- Source: <https://github.com/bytecodealliance/rustix>
- Version: 1.1.4
- License: Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

## zbus

The daemon uses the safe Rust `zbus` client to create and verify transient
systemd user-manager slices and scopes before terminal commands execute.

- Source: <https://github.com/z-galaxy/zbus>
- Resolved version: 5.19.0
- License: MIT
- Transport: the existing per-user D-Bus session bus only
- No first-party unsafe block or direct cgroup-filesystem mutation is used

## fontdb

Splinterm's Roadmap Phase 2 font discovery spike uses `fontdb` to inventory and
query installed font faces.

- Source: <https://github.com/RazrFalcon/fontdb>
- Version: 0.23.0
- License: MIT

## swash

The font-stack spike uses Swash to inspect OpenType metrics and character-map
coverage. Shaping and raster suitability remain under evaluation.

- Source: <https://github.com/dfrg/swash>
- Version: 0.2.10
- License: Apache-2.0 OR MIT

## FreeType and freetype-rs

Phase 8.1 uses the system FreeType library through the safe `freetype-rs`
wrapper in the dedicated `splinterm-freetype` crate. The bridge reproduces the
pinned Foot/fcft light-hinted normal-grayscale raster path and returns only
bounded owned glyph data.

- FreeType source: <https://freetype.org/>
- Reference-host FreeType version: 2.14.1 (`pkg-config` 26.6.20)
- FreeType license: FreeType License (FTL) OR GPL-2.0-only
- freetype-rs source: <https://github.com/PistonDevelopers/freetype-rs>
- freetype-rs version: 0.38.0
- freetype-rs license: MIT
- freetype-sys version: 0.23.0
- Linkage: dynamic system FreeType discovered through `pkg-config`

No first-party unsafe block is used. Exact raster fixtures remain sensitive to
the FreeType build, font file, face index, fontconfig policy, and pixel size.

## smithay-client-toolkit

Splinterm's Roadmap Phase 2 native Wayland mechanism spike uses
`smithay-client-toolkit` for registry dispatch, xdg-shell lifecycle, seat/output
state, keyboard integration, SHM slot pooling, and calloop integration.

- Source: <https://github.com/smithay/client-toolkit>
- Version: 0.20.0
- License: MIT

## wayland-client

The native Wayland mechanism spike uses the Rust `wayland-client` bindings for
Wayland connections and protocol objects.

- Source: <https://github.com/Smithay/wayland-rs>
- Version: 0.31.14
- License: MIT

## unicode-width

Splinterm uses `unicode-width` for safe, deterministic Unicode display-width
classification without FFI or unsafe code.

- Source: <https://github.com/unicode-rs/unicode-width>
- Version: 0.2.2
- License: MIT OR Apache-2.0

## Tabler Icons

The graphical client embeds a 23-glyph subset of the Tabler Icons outline and
filled webfonts in the `splinterm` executable for agent status, agent access,
Lair retention, Explorer disclosure, and Explorer current-row marker icons.
The subset is drawn only by trusted interface code; it is not installed as a
font and never renders terminal content.

- Source: <https://github.com/tabler/tabler-icons>, npm package
  `@tabler/icons-webfont`
- Version: 3.49.0
- License: MIT
- Embedded file and provenance: `crates/splinterm/assets/icons/`, including the
  original and subset SHA-256 digests and the exact `pyftsubset` command

MIT License

Copyright (c) 2020-2026 Paweł Kuna

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
