# Interface icons

`tabler-icons-subset.ttf` is one subset face built from the Tabler Icons
outline and filled webfonts and, for the agent access robots, the Material
Design Icons webfont. It is embedded in the `splinterm` executable with
`include_bytes!` and drawn through the renderer's FreeType path for agent
status, agent access, Lair retention, Explorer disclosure, and the Explorer's
current-row marker (`[explorer] current-marker`). It is never installed as a
system font and never used for terminal content.

## Tabler Icons

- Source package: `@tabler/icons-webfont` 3.49.0 from the npm registry,
  <https://registry.npmjs.org/@tabler/icons-webfont/-/icons-webfont-3.49.0.tgz>
  (tarball SHA-256
  `48c69c65a5f9013d482a17cab8899a09dcaec50750d1ac7324eb63a061fffb82`)
- Original files:
  - `package/dist/fonts/tabler-icons.ttf` (outline), SHA-256
    `d291676dc4c28b386552753bf16cf2bef0e93e5543f8e0b5c5d72c0da41ae1bc`
  - `package/dist/fonts/tabler-icons-filled.ttf` (filled), SHA-256
    `08304212f3f43271ee700156044a298a081ad2a24b30e28b4f3346dd2fe7ecf9`
- Project: <https://tabler.io/icons>, <https://github.com/tabler/tabler-icons>
- License: MIT, Copyright (c) 2020-2026 Paweł Kuna; see [`LICENSE`](LICENSE).

## Material Design Icons

- Source package: `@mdi/font` 7.4.47 from the npm registry,
  <https://registry.npmjs.org/@mdi/font/-/font-7.4.47.tgz> (tarball SHA-256
  `66131558352dc8df724feac30f3b7bf7ee7311a79e4f8436f3637c0993edb773`)
- Original file: `package/fonts/materialdesignicons-webfont.ttf`, SHA-256
  `61e8aba5a4e981fe22cf7c8e8bcdbea00476e75c62c37f01bf7ee33361d68428`
- The three icons used, `robot`, `robot-outline`, and `robot-off-outline`,
  were drawn for Material Design Icons by its contributors (GreenTurtwig;
  Colton Wiscombe), so they are under the collection's Apache 2.0 license.
  Nerd Fonts carries the same icons as `nf-md-robot`, `nf-md-robot_outline`,
  and `nf-md-robot_off_outline`.
- Project: <https://pictogrammers.com/library/mdi/>,
  <https://github.com/Templarian/MaterialDesign-Webfont>
- License: Apache 2.0, Pictogrammers; see [`LICENSE-MDI`](LICENSE-MDI), the
  package's license followed by the Apache License 2.0 text.

Both notices are also carried in the repository's `THIRD_PARTY.md`, which
every package installs under `share/licenses`.

## Subset file

- SHA-256 `45fd56e3898bcb7257aceaf2cc673c472f424b1d29eb1fc97adcdaa4c7fecd6d`
  (6088 bytes)

## Subset commands

Run with fontTools 4.63.0 (`nix-shell -p python3Packages.fonttools`), with the
Tabler fonts from `package/dist/fonts` and the Material Design font from
`package/fonts` in the current directory; the output is byte-for-byte
reproducible.

The two Tabler faces have the same units per em (1000) and hhea and
typographic ascent and descent (900 and 100), and no codepoint in common, so
they merge into one face; they differ only in the OS/2 Windows descent (104
outline, 100 filled), where the merge keeps the larger, 104.

Material Design Icons uses 512 units per em, with an ascent of 448 and a
descent of 64, and draws on the same 24-unit grid as Tabler. Its subset is
scaled to 1000 units per em, raised by 25 units so the grid's centre sits
where Tabler's does (400 units above the baseline), and given Tabler's
vertical metrics, so it merges like a third Tabler face. Its codepoints are in
Supplementary Private Use Area-A and share none with Tabler.

Subset each face, merge them, then subset the merge once more to drop the
extra `.notdef` glyphs. `pyftmerge` stamps the merged face with the current
time unless `SOURCE_DATE_EPOCH` is set; `0` keeps the original faces'
timestamps.

`pyftmerge` also recalculates each glyph's bounding box. Tabler's faces store
a left edge of 0 for every glyph, matching its left side bearing of 0, while
the outline sits centred in the advance. FreeType shifts a glyph left by the
difference between its stored left edge and its left side bearing, so after
the merge every icon would be drawn off centre. The last step sets each left
side bearing to the recalculated left edge, which renders every icon exactly
as the original faces do.

```sh
pyftsubset tabler-icons.ttf \
  --unicodes=U+F98B,U+EA05,U+EA67,U+EB0C,U+EA70,U+EA61,U+EA5F,U+EC9C,U+ED60,U+F931,U+EA9A,U+ECEF,U+EBD3,U+EC3E,U+EA1C,U+EA7D \
  --output-file=outline.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
pyftsubset tabler-icons-filled.ttf \
  --unicodes=U+FB2C,U+F7D8,U+F691,U+F680,U+F705 \
  --output-file=filled.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
pyftsubset materialdesignicons-webfont.ttf \
  --unicodes=U+F06A9,U+F167A,U+F167B \
  --output-file=mdi-raw.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
python3 -c 'import sys
from fontTools.ttLib import TTFont
from fontTools.ttLib.scaleUpem import scale_upem
font = TTFont(sys.argv[1], recalcTimestamp=False)
scale_upem(font, 1000)
glyf = font["glyf"]
for name in font.getGlyphOrder():
    glyph = glyf[name]
    if glyph.numberOfContours > 0:
        coordinates, _, _ = glyph.getCoordinates(glyf)
        glyph.coordinates = coordinates.__class__([(x, y + 25) for x, y in coordinates])
        glyph.recalcBounds(glyf)
hhea, os2 = font["hhea"], font["OS/2"]
hhea.ascent, hhea.descent, hhea.lineGap = 900, -100, 0
os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = 900, -100, 0
os2.usWinAscent, os2.usWinDescent = 990, 104
font.save(sys.argv[2])' mdi-raw.ttf mdi.ttf
SOURCE_DATE_EPOCH=0 pyftmerge outline.ttf filled.ttf mdi.ttf --output-file=merged.ttf
pyftsubset merged.ttf --unicodes="*" \
  --output-file=subset.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
python3 -c 'import sys
from fontTools.ttLib import TTFont
font = TTFont(sys.argv[1], recalcTimestamp=False)
for name in font.getGlyphOrder():
    advance, _ = font["hmtx"][name]
    font["hmtx"][name] = (advance, getattr(font["glyf"][name], "xMin", 0))
font.save(sys.argv[2])' subset.ttf tabler-icons-subset.ttf
```

## Codepoints

Tabler codepoints come from the package's `dist/tabler-icons.css` (outline)
and `dist/tabler-icons-filled.css` (filled, where the class drops the
`-filled` suffix, for example `.ti-caret-right`); Material Design codepoints
come from its `css/materialdesignicons.css`. They are the projects' own
codepoints, mostly private-use (U+F98B falls in the CJK Compatibility
Ideographs block and U+FB2C in Alphabetic Presentation Forms), used inside
this embedded face only: Splinterm places icons from trusted interface code
and never interprets these codepoints in labels, titles, agent detail text,
or terminal output.

| Icon | Face | Codepoint | Use |
| --- | --- | --- | --- |
| `message-question` | Tabler outline | U+F98B | agent needs input |
| `alert-circle` | Tabler outline | U+EA05 | agent error |
| `circle-check` | Tabler outline | U+EA67 | agent done |
| `point` | Tabler outline | U+EB0C | agent idle |
| `clock` | Tabler outline | U+EA70 | stale agent status |
| `chevron-right` | Tabler outline | U+EA61 | collapsed Explorer row |
| `chevron-down` | Tabler outline | U+EA5F | expanded Explorer row |
| `pin` | Tabler outline | U+EC9C | saved Lair |
| `pinned` | Tabler outline | U+ED60 | pinned Lair |
| `robot-outline` | Material Design | U+F167A | an agent can reach the row |
| `robot` | Material Design | U+F06A9 | an agent is connected to the row |
| `robot-off-outline` | Material Design | U+F167B | an agent is connected, but nothing covers the row |
| `lock-question` | Tabler outline | U+F931 | an agent is asking for access |
| `eye` | Tabler outline | U+EA9A | current row (the default marker) |
| `current-location` | Tabler outline | U+ECEF | current row marker |
| `focus-2` | Tabler outline | U+EBD3 | current row marker |
| `crosshair` | Tabler outline | U+EC3E | current row marker |
| `arrow-narrow-right` | Tabler outline | U+EA1C | current row marker |
| `corner-down-right` | Tabler outline | U+EA7D | current row marker |
| `caret-right-filled` | Tabler filled | U+FB2C | current row marker |
| `arrow-badge-right-filled` | Tabler filled | U+F7D8 | current row marker |
| `player-play-filled` | Tabler filled | U+F691 | current row marker |
| `map-pin-filled` | Tabler filled | U+F680 | current row marker |
| `circle-dot-filled` | Tabler filled | U+F705 | current row marker |

The star and bookmark icons are left out on purpose and kept for later use.

The working state has no icon: it is a Braille spinner the renderer draws
itself, so it needs neither this face nor a font.

To add an icon, add its codepoint to the command for its face above,
regenerate the subset from the same package versions, update the hashes and
this table, and add the icon to `ChromeIcon` in
`crates/splinterm/src/renderer/icons.rs`.
