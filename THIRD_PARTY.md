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

The graphical client embeds a 21-glyph subset of the Tabler Icons outline and
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

## Material Design Icons

The graphical client embeds three Material Design Icons glyphs, `robot`,
`robot-outline`, and `robot-off-outline`, in the same subset face as the Tabler
Icons, for the Explorer's agent access marks. They are drawn only by trusted
interface code; they are not installed as a font and never render terminal
content.

- Source: <https://github.com/Templarian/MaterialDesign-Webfont>, npm package
  `@mdi/font`
- Version: 7.4.47
- License: Apache-2.0
- Embedded file and provenance: `crates/splinterm/assets/icons/`, including the
  original and subset SHA-256 digests and the exact commands
- Changes: the three glyphs are subset from the original font, scaled from 512
  to 1000 units per em, raised 25 units, given the Tabler faces' vertical
  metrics, and merged into the Tabler subset face

Pictogrammers Free License
--------------------------

This icon collection is released as free, open source, and GPL friendly by
the [Pictogrammers](http://pictogrammers.com/) icon group. You may use it
for commercial projects, open source projects, or anything really.

# Icons: Apache 2.0 (https://www.apache.org/licenses/LICENSE-2.0)
Some of the icons are redistributed under the Apache 2.0 license. All other
icons are either redistributed under their respective licenses or are
distributed under the Apache 2.0 license.

# Fonts: Apache 2.0 (https://www.apache.org/licenses/LICENSE-2.0)
All web and desktop fonts are distributed under the Apache 2.0 license. Web
and desktop fonts contain some icons that are redistributed under the Apache
2.0 license. All other icons are either redistributed under their respective
licenses or are distributed under the Apache 2.0 license.

# Code: MIT (https://opensource.org/licenses/MIT)
The MIT license applies to all non-font and non-icon files.

---


                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright [yyyy] [name of copyright owner]

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
