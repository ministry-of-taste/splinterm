# Interface icons

`tabler-icons-subset.ttf` is one subset face built from the Tabler Icons
outline and filled webfonts. It is embedded in the `splinterm` executable with
`include_bytes!` and drawn through the renderer's FreeType path for agent
status, agent access, Lair retention, Explorer disclosure, and the Explorer's
current-row marker (`[explorer] current-marker`). It is never installed as a
system font and never used for terminal content.

- Source package: `@tabler/icons-webfont` 3.49.0 from the npm registry,
  <https://registry.npmjs.org/@tabler/icons-webfont/-/icons-webfont-3.49.0.tgz>
  (tarball SHA-256
  `48c69c65a5f9013d482a17cab8899a09dcaec50750d1ac7324eb63a061fffb82`)
- Original files:
  - `package/dist/fonts/tabler-icons.ttf` (outline), SHA-256
    `d291676dc4c28b386552753bf16cf2bef0e93e5543f8e0b5c5d72c0da41ae1bc`
  - `package/dist/fonts/tabler-icons-filled.ttf` (filled), SHA-256
    `08304212f3f43271ee700156044a298a081ad2a24b30e28b4f3346dd2fe7ecf9`
- Subset file SHA-256:
  `325ccab6940b2316b7dfd7ad56e671de0232d948ef9e11a500bd78f7b86f684e`
  (5712 bytes)
- Project: <https://tabler.io/icons>, <https://github.com/tabler/tabler-icons>
- License: MIT, Copyright (c) 2020-2026 Paweł Kuna; see [`LICENSE`](LICENSE).
  The notice is also carried in the repository's `THIRD_PARTY.md`, which every
  package installs under `share/licenses`.

## Subset commands

Run in `package/dist/fonts` with fontTools 4.63.0
(`nix-shell -p python3Packages.fonttools`); the output is byte-for-byte
reproducible. The two faces have the same units per em (1000) and hhea and
typographic ascent and descent (900 and 100), and no codepoint in common, so
they merge into one face; they differ only in the OS/2 Windows descent (104
outline, 100 filled), where the merge keeps the larger, 104. Subset each face,
merge them, then subset the merge once more to drop the second `.notdef`.
`pyftmerge` stamps the merged face with the current time unless
`SOURCE_DATE_EPOCH` is set; `0` keeps the original faces' timestamps.

`pyftmerge` also recalculates each glyph's bounding box. Tabler's faces store
a left edge of 0 for every glyph, matching its left side bearing of 0, while
the outline sits centred in the advance. FreeType shifts a glyph left by the
difference between its stored left edge and its left side bearing, so after
the merge every icon would be drawn off centre. The last step sets each left
side bearing to the recalculated left edge, which renders every icon exactly
as the original faces do.

```sh
pyftsubset tabler-icons.ttf \
  --unicodes=U+F98B,U+EA05,U+EA67,U+EB0C,U+EA70,U+EA61,U+EA5F,U+EC9C,U+ED60,U+EBD9,U+F00A,U+F931,U+EA9A,U+ECEF,U+EBD3,U+EC3E,U+EA1C,U+EA7D \
  --output-file=outline.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
pyftsubset tabler-icons-filled.ttf \
  --unicodes=U+FB2C,U+F7D8,U+F691,U+F680,U+F705 \
  --output-file=filled.ttf \
  --layout-features= --no-hinting --desubroutinize --name-IDs="*" \
  --notdef-outline --drop-tables+=FFTM
SOURCE_DATE_EPOCH=0 pyftmerge outline.ttf filled.ttf --output-file=merged.ttf
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

Codepoints come from the package's `dist/tabler-icons.css` (outline) and
`dist/tabler-icons-filled.css` (filled, where the class drops the `-filled`
suffix, for example `.ti-caret-right`). They are Tabler-assigned codepoints,
mostly private-use (U+F98B falls in the CJK Compatibility Ideographs block and
U+FB2C in Alphabetic Presentation Forms), used inside this embedded face only:
Splinterm places icons from trusted interface code and never interprets these
codepoints in labels, titles, agent detail text, or terminal output.

| Icon | Face | Codepoint | Use |
| --- | --- | --- | --- |
| `message-question` | outline | U+F98B | agent needs input |
| `alert-circle` | outline | U+EA05 | agent error |
| `circle-check` | outline | U+EA67 | agent done |
| `point` | outline | U+EB0C | agent idle |
| `clock` | outline | U+EA70 | stale agent status |
| `chevron-right` | outline | U+EA61 | collapsed Explorer row |
| `chevron-down` | outline | U+EA5F | expanded Explorer row |
| `pin` | outline | U+EC9C | saved Lair |
| `pinned` | outline | U+ED60 | pinned Lair |
| `plug` | outline | U+EBD9 | an agent can reach the row |
| `plug-connected` | outline | U+F00A | an agent is connected to the row |
| `lock-question` | outline | U+F931 | an agent is asking for access |
| `eye` | outline | U+EA9A | current row (the default marker) |
| `current-location` | outline | U+ECEF | current row marker |
| `focus-2` | outline | U+EBD3 | current row marker |
| `crosshair` | outline | U+EC3E | current row marker |
| `arrow-narrow-right` | outline | U+EA1C | current row marker |
| `corner-down-right` | outline | U+EA7D | current row marker |
| `caret-right-filled` | filled | U+FB2C | current row marker |
| `arrow-badge-right-filled` | filled | U+F7D8 | current row marker |
| `player-play-filled` | filled | U+F691 | current row marker |
| `map-pin-filled` | filled | U+F680 | current row marker |
| `circle-dot-filled` | filled | U+F705 | current row marker |

The star and bookmark icons are left out on purpose and kept for later use.

The working state has no icon: it is a Braille spinner the renderer draws
itself, so it needs neither this face nor a font.

To add an icon, add its codepoint to the command for its face above,
regenerate the subset from the same package version, update the hashes and
this table, and add the icon to `ChromeIcon` in
`crates/splinterm/src/renderer/icons.rs`.
