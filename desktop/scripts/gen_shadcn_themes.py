#!/usr/bin/env python3
"""Make crates/app/src/shadcn_themes.rs from the shadcn/ui theme registry.

Usage:
  gh api repos/shadcn-ui/ui/contents/apps/v4/registry/themes.ts \\
      -H "Accept: application/vnd.github.raw" > /tmp/themes.ts
  scripts/gen_shadcn_themes.py /tmp/themes.ts

The registry gives colors in OKLCH. This script changes them to sRGB (0xRRGGBBAA),
so that the app does no color math at runtime.
"""

import math
import re
import sys
from pathlib import Path

# The themes that set all colors. The other themes set only the accent colors.
BASES = ["neutral", "stone", "zinc", "mauve", "olive", "mist", "taupe"]

OUT = Path(__file__).resolve().parent.parent / "crates/app/src/shadcn_themes.rs"


def oklch_to_rgba(value: str) -> int:
    m = re.fullmatch(r"oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*(?:/\s*([\d.]+)%)?\s*\)", value.strip())
    if not m:
        raise ValueError(f"not an oklch color: {value}")
    l, c, h = float(m[1]), float(m[2]), float(m[3])
    alpha = float(m[4]) / 100 if m[4] else 1.0
    a, b = c * math.cos(math.radians(h)), c * math.sin(math.radians(h))
    l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3
    m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3
    s_ = (l - 0.0894841775 * a - 1.2914855480 * b) ** 3
    linear = (
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    )

    def encode(x: float) -> int:
        x = min(max(x, 0.0), 1.0)
        x = 12.92 * x if x <= 0.0031308 else 1.055 * x ** (1 / 2.4) - 0.055
        return round(x * 255)

    r, g, bl = (encode(x) for x in linear)
    return (r << 24) | (g << 16) | (bl << 8) | round(alpha * 255)


def parse(source: str):
    """Return [(name, title, {mode: {var: color}})] in the order of the registry."""
    themes = []
    for block in re.split(r"\n  \{\n", source)[1:]:
        name = re.search(r'name: "([^"]+)"', block)[1]
        title = re.search(r'title: "([^"]+)"', block)[1]
        modes = {}
        for mode in ("light", "dark"):
            body = re.search(rf"\b{mode}: \{{(.*?)\n      \}}", block, re.S)[1]
            modes[mode] = {
                k.strip('"'): oklch_to_rgba(v)
                for k, v in re.findall(r'^\s*("?[\w-]+"?): "(oklch\([^"]+\))"', body, re.M)
            }
        themes.append((name, title, modes))
    return themes


def rust_vars(colors: dict) -> str:
    rows = "".join(f'            ("{k}", 0x{v:08x}),\n' for k, v in colors.items())
    return f"&[\n{rows}        ]"


def rust_theme(name: str, title: str, modes: dict) -> str:
    return (
        "    ThemeDef {\n"
        f'        key: "{name}",\n'
        f'        title: "{title}",\n'
        f"        light: {rust_vars(modes['light'])},\n"
        f"        dark: {rust_vars(modes['dark'])},\n"
        "    },\n"
    )


def main() -> None:
    themes = parse(Path(sys.argv[1]).read_text())
    bases = [t for t in themes if t[0] in BASES]
    accents = [t for t in themes if t[0] not in BASES]
    if len(bases) != len(BASES):
        raise SystemExit(f"found {len(bases)} base themes, expected {len(BASES)}")
    out = [
        "//! The shadcn/ui themes, from `apps/v4/registry/themes.ts` in shadcn-ui/ui.\n",
        "//! Do not edit. `scripts/gen_shadcn_themes.py` makes this file.\n\n",
        "use crate::theme::ThemeDef;\n\n",
        "/// The base colors. Each one sets all the colors of the app.\n",
        "pub const BASES: &[ThemeDef] = &[\n",
        *(rust_theme(*t) for t in bases),
        "];\n\n",
        "/// The accent colors. Each one sets the primary colors on top of a base color.\n",
        "pub const ACCENTS: &[ThemeDef] = &[\n",
        *(rust_theme(*t) for t in accents),
        "];\n",
    ]
    OUT.write_text("".join(out))
    print(f"wrote {OUT}: {len(bases)} base colors, {len(accents)} accent colors")


if __name__ == "__main__":
    main()
