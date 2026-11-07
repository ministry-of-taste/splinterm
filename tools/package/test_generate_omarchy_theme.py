"""Omarchy 4 parity for the shipped tools/generate-omarchy-theme.py exporter."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
EXPORTER = ROOT / "tools/generate-omarchy-theme.py"
FIXTURES = ROOT / "crates/splinterm/tests/fixtures/omarchy4"

# The fixtures are verbatim stock Omarchy 4 colors.toml files (4a9c83de), the
# pre-rename gruvbox (afa2839a^), and an omarchy-theme-colors-from-alacritty
# conversion. Each foot.ini is the stock template rendered by Omarchy's own
# resolver, so its palette is what Omarchy itself produces.
SELECTED_TAB = {
    "tokyo-night": "#24283b",
    "flexoki-light": "#E6E4D9",
    "last-horizon": "#a5a0b6",
    "legacy-gruvbox": "#3c3836",
    # No lighter_background or lighter_bg: Splinterm's color8 ramp.
    "alacritty-kanagawa": "#727169",
}


def load_exporter():
    spec = importlib.util.spec_from_file_location("generate_omarchy_theme", EXPORTER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def foot_palette(path: Path) -> dict[str, str]:
    palette = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        key, separator, value = line.partition("=")
        if separator and not key.startswith("#"):
            palette[key.strip()] = "#" + value.strip().split()[-1].lower()
    return palette


class GenerateOmarchyThemeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.exporter = load_exporter()

    def colors(self, name: str) -> dict[str, object]:
        with (FIXTURES / name / "colors.toml").open("rb") as source:
            return tomllib.load(source)

    def test_stock_themes_export_authored_tabs_and_the_omarchy_palette(self) -> None:
        for name, selected_tab in SELECTED_TAB.items():
            with self.subTest(theme=name):
                generated = self.exporter.generate(self.colors(name))
                self.assertEqual(generated["active_tab_background"], selected_tab)
                foot = foot_palette(FIXTURES / name / "foot.ini")
                self.assertEqual(generated["background"].lower(), foot["background"])
                self.assertEqual(generated["foreground"].lower(), foot["foreground"])
                self.assertEqual(
                    generated["selection"].lower(), foot["selection-background"]
                )
                self.assertEqual(
                    generated["selection_foreground"].lower(),
                    foot["selection-foreground"],
                )
                ansi = [color.lower() for color in generated["ansi"]]
                for index in range(1, 8):
                    self.assertEqual(ansi[index], foot[f"regular{index}"], index)
                for index in range(8):
                    self.assertEqual(ansi[index + 8], foot[f"bright{index}"], index + 8)

    def test_canonical_names_win_and_empty_values_fall_through(self) -> None:
        colors = self.colors("tokyo-night")
        both = self.exporter.generate(
            {**colors, "lighter_bg": "#111111", "bg": "#222222", "bright_fg": "#333333"}
        )
        self.assertEqual(both["active_tab_background"], "#24283b")
        self.assertEqual(both["background"], "#1a1b26")
        self.assertEqual(both["ansi"][15], "#c0caf5")

        legacy = self.colors("legacy-gruvbox")
        self.assertNotIn("lighter_background", legacy)
        self.assertEqual(self.exporter.generate(legacy)["ansi"][15], "#d4be98")

        fallthrough = self.exporter.generate(
            {**colors, "lighter_background": "", "lighter_bg": "#444444", "muted": ""}
        )
        self.assertEqual(fallthrough["active_tab_background"], "#444444")
        self.assertEqual(fallthrough["pane_border"], "#565f89")

    def test_explicit_active_tab_background_and_long_form_only_colors(self) -> None:
        colors = {
            key: value for key, value in self.colors("tokyo-night").items()
            if not key.startswith("color")
            and key not in {"bg", "fg", "darker_bg", "lighter_bg", "bright_fg"}
        }
        colors.update({
            "background": "#000000",
            "foreground": "#E8E8ED",
            "darker_background": "#000000",
            "lighter_background": "#1B1B20",
            "bright_foreground": "#FAFAFC",
        })
        self.assertEqual(self.exporter.generate(colors)["active_tab_background"], "#1B1B20")
        for color in ("#1C1C22", "#f8f9fa"):
            with self.subTest(color=color):
                generated = self.exporter.generate({**colors, "active_tab_background": color})
                self.assertEqual(generated["active_tab_background"], color)
                self.assertEqual(generated["ansi"][0], "#000000")
                self.assertEqual(generated["ansi"][15], "#FAFAFC")
                self.assertEqual(
                    generated["active_tab_foreground"],
                    self.exporter.active_tab_foreground_fallback(
                        colors["background"], colors["foreground"], color
                    ),
                )
        empty = self.exporter.generate({**colors, "active_tab_background": ""})
        self.assertEqual(empty["active_tab_background"], "#1B1B20")
        for invalid in ("not-a-color", "#12345g", "#123", 123):
            with self.subTest(invalid=invalid):
                with self.assertRaisesRegex(ValueError, "active_tab_background must be #RRGGBB"):
                    self.exporter.generate({**colors, "active_tab_background": invalid})

    def test_missing_bright_accents_are_derived_like_omarchy(self) -> None:
        colors = dict(self.colors("tokyo-night"))
        del colors["bright_red"]
        generated = self.exporter.generate(colors)
        self.assertEqual(generated["ansi"][9], "#f991a5")
        self.assertEqual(self.exporter.mix_color("#000000", "#ffffff", 0.2), "#333333")

    def test_foot_colors_section_merges_with_an_appended_glass_block(self) -> None:
        glass = FIXTURES / "glass"
        with tempfile.TemporaryDirectory() as directory:
            theme = Path(directory)
            for fixture, expected in (
                ("legacy-colors.foot.ini", (1.0, False)),
                ("legacy-colors.glassy.foot.ini", (0.65, True)),
            ):
                (theme / "foot.ini").write_text(
                    (glass / fixture).read_text(encoding="utf-8"), encoding="utf-8"
                )
                self.assertEqual(self.exporter.theme_settings(theme), expected, fixture)
            (theme / "foot.ini").write_text(
                "initial-color-theme=light\n[colors2]\nalpha=0.4\n"
                "[colors-dark]\nalpha=0.9\n[colors-light]\nblur=yes\n",
                encoding="utf-8",
            )
            self.assertEqual(self.exporter.theme_settings(theme), (0.4, True))
            (theme / "foot.ini").write_text(
                (FIXTURES / "tokyo-night/foot.ini").read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            self.assertEqual(self.exporter.theme_settings(theme), (1.0, True))

    def test_command_line_exports_a_stock_theme(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "theme.json"
            subprocess.run(
                [
                    sys.executable,
                    "-I",
                    str(EXPORTER),
                    str(FIXTURES / "tokyo-night/colors.toml"),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            generated = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(generated["active_tab_background"], "#24283b")
            self.assertEqual((generated["alpha"], generated["blur"]), (1.0, True))


if __name__ == "__main__":
    unittest.main()
