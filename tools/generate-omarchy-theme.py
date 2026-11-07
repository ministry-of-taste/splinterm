#!/usr/bin/env python3
"""Generate Splinterm's bounded theme JSON from an Omarchy colors.toml file."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import tempfile
import tomllib

# Each role lists the canonical Omarchy 4 colors.toml name first, then the
# legacy short name and ANSI aliases in the order omarchy-theme-color resolves
# them. The selected-tab role keeps Splinterm's color8 ramp rather than
# Omarchy's color0/background last resort, which would hide the selected tab.
ROLE_ALIASES = {
    "accent": ("accent", "cursor", "blue", "color4"),
    "bg": ("background", "bg", "color0"),
    "darker_bg": ("darker_background", "darker_bg", "color0"),
    "lighter_bg": ("lighter_background", "lighter_bg", "color8"),
    "selection": (
        "selection", "selection_background", "color8", "color0", "background", "bg",
    ),
    "muted": ("muted", "color8", "dark_foreground", "dark_fg"),
    "fg": ("foreground", "fg", "color7"),
    "bright_fg": ("bright_foreground", "bright_fg", "color15", "foreground", "fg"),
    "red": ("red", "color1"),
    "green": ("green", "color2"),
    "yellow": ("yellow", "color3"),
    "blue": ("blue", "color4"),
    "magenta": ("magenta", "color5", "purple"),
    "cyan": ("cyan", "color6"),
    "bright_red": ("bright_red", "color9"),
    "bright_green": ("bright_green", "color10"),
    "bright_yellow": ("bright_yellow", "color11"),
    "bright_blue": ("bright_blue", "color12"),
    "bright_magenta": ("bright_magenta", "color13", "bright_purple"),
    "bright_cyan": ("bright_cyan", "color14"),
}

# Omarchy derives a missing bright accent by mixing its base 20% toward white.
DERIVED_BRIGHT_ROLES = {
    "bright_red": "red",
    "bright_green": "green",
    "bright_yellow": "yellow",
    "bright_blue": "blue",
    "bright_magenta": "magenta",
    "bright_cyan": "cyan",
}


def resolve_role(colors: dict[str, object], aliases: tuple[str, ...]) -> object:
    """Returns the first non-empty alias, as omarchy-theme-color does."""
    return next(
        (colors[key] for key in aliases if key in colors and colors[key] != ""),
        None,
    )


def mix_color(start: str, end: str, amount: float) -> str:
    """Mixes two #RRGGBB colors exactly like omarchy-theme-color's mix_color."""
    channels = []
    for index in (1, 3, 5):
        first = int(start[index : index + 2], 16)
        second = int(end[index : index + 2], 16)
        channels.append(int(first * (1 - amount) + second * amount + 0.5))
    return "#{:02x}{:02x}{:02x}".format(*channels)


def parse_foot_bool(value: str) -> bool:
    normalized = value.strip().lower()
    if normalized in {"yes", "true", "on", "1"}:
        return True
    if normalized in {"no", "false", "off", "0"}:
        return False
    raise ValueError("foot.ini blur must be a boolean")


def theme_settings(theme_dir: Path) -> tuple[float, bool]:
    foot = theme_dir / "foot.ini"
    if not foot.is_file():
        return 1.0, False

    # Foot 1.27.0 parses the deprecated [colors] into the same dark theme as
    # [colors-dark], and [colors2] into [colors-light], key by key in file
    # order, so the sections merge and the last assignment wins.
    themes: dict[str, dict[str, str]] = {"main": {}, "dark": {}, "light": {}}
    section_theme = {
        "main": "main",
        "colors": "dark",
        "colors-dark": "dark",
        "colors2": "light",
        "colors-light": "light",
    }
    section = "main"
    for raw_line in foot.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip().lower()
            continue
        if section not in section_theme:
            continue
        key, separator, value = line.partition("=")
        normalized_key = key.strip().lower()
        if separator and normalized_key in {"alpha", "blur", "initial-color-theme"}:
            themes[section_theme[section]][normalized_key] = value.strip()

    initial = themes["main"].get("initial-color-theme", "dark")
    if initial not in {"dark", "light"}:
        raise ValueError("foot.ini initial-color-theme must be dark or light")
    values = themes[initial]
    try:
        alpha = float(values.get("alpha", "1.0"))
    except ValueError as error:
        raise ValueError("foot.ini alpha must be a number") from error
    if not 0.0 <= alpha <= 1.0:
        raise ValueError("foot.ini alpha must be between 0.0 and 1.0")
    blur = parse_foot_bool(values["blur"]) if "blur" in values else False
    return alpha, blur


def relative_luminance(color: str) -> float:
    channels = [int(color[index : index + 2], 16) / 255.0 for index in (1, 3, 5)]
    linear = [
        channel / 12.92
        if channel <= 0.04045
        else ((channel + 0.055) / 1.055) ** 2.4
        for channel in channels
    ]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


def contrast_ratio(first: str, second: str) -> float:
    first_luminance = relative_luminance(first)
    second_luminance = relative_luminance(second)
    lighter = max(first_luminance, second_luminance)
    darker = min(first_luminance, second_luminance)
    return (lighter + 0.05) / (darker + 0.05)


def active_tab_foreground_fallback(
    background: str, foreground: str, active_tab_background: str
) -> str:
    if contrast_ratio(background, active_tab_background) > contrast_ratio(
        foreground, active_tab_background
    ):
        return background
    return foreground


def validate_role(name: str, value: object) -> None:
    if not isinstance(value, str) or len(value) != 7 or not value.startswith("#"):
        raise ValueError(f"{name} must be #RRGGBB")
    try:
        int(value[1:], 16)
    except ValueError as error:
        raise ValueError(f"{name} must be #RRGGBB") from error


def generate(
    colors: dict[str, object], alpha: float = 1.0, blur: bool = False
) -> dict[str, object]:
    roles = {
        role: resolve_role(colors, aliases) for role, aliases in ROLE_ALIASES.items()
    }
    for bright, base in DERIVED_BRIGHT_ROLES.items():
        if roles[bright] is None and roles[base] is not None:
            validate_role(base, roles[base])
            roles[bright] = mix_color(str(roles[base]), "#ffffff", 0.2)
    selection_foreground = resolve_role(colors, ("selection_foreground",))
    roles["selection_foreground"] = (
        roles["bright_fg"] if selection_foreground is None else selection_foreground
    )
    active_tab_background = resolve_role(colors, ("active_tab_background",))
    roles["active_tab_background"] = (
        roles["lighter_bg"] if active_tab_background is None else active_tab_background
    )
    missing = [name for name, value in roles.items() if value is None]
    if missing:
        raise ValueError("missing Omarchy roles: " + ", ".join(missing))
    for name, value in roles.items():
        validate_role(name, value)
    active_tab_foreground = active_tab_foreground_fallback(
        roles["bg"], roles["fg"], roles["active_tab_background"]
    )
    roles["active_tab_foreground"] = active_tab_foreground
    if not 0.0 <= alpha <= 1.0:
        raise ValueError("alpha must be between 0.0 and 1.0")
    if not isinstance(blur, bool):
        raise ValueError("blur must be a boolean")
    return {
        "background": roles["bg"],
        "alpha": float(alpha),
        "blur": blur,
        "foreground": roles["fg"],
        "cursor": roles["accent"],
        "selection": roles["selection"],
        "selection_foreground": roles["selection_foreground"],
        "active_tab_background": roles["active_tab_background"],
        "active_tab_foreground": roles["active_tab_foreground"],
        "url": roles["blue"],
        "ui_accent": roles["accent"],
        "pane_border": roles["muted"],
        "pane_border_active": roles["accent"],
        "ansi": [
            roles["darker_bg"], roles["red"], roles["green"], roles["yellow"],
            roles["blue"], roles["magenta"], roles["cyan"], roles["fg"],
            roles["muted"], roles["bright_red"], roles["bright_green"],
            roles["bright_yellow"], roles["bright_blue"], roles["bright_magenta"],
            roles["bright_cyan"], roles["bright_fg"],
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path, help="Omarchy theme colors.toml")
    parser.add_argument(
        "--output", type=Path,
        default=Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "splinterm/theme.json",
    )
    args = parser.parse_args()
    with args.source.open("rb") as source:
        alpha, blur = theme_settings(args.source.parent)
        generated = generate(tomllib.load(source), alpha, blur)
    args.output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix=".theme.", dir=args.output.parent, text=True)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as target:
            json.dump(generated, target, indent=2)
            target.write("\n")
        os.replace(temporary, args.output)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
