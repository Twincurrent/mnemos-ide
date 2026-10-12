# SPDX-License-Identifier: AGPL-3.0-only
"""Generates app/assets/bundled/fonts/password.ttf, the font password fields draw with.

Mnemos IDE: the font this replaces had no recorded origin or license. This one is
TwinCurrent ApS's own work, under the same license as the rest of Mnemos, and this script is
its source. It has one glyph, a filled circle, and maps every code point to it (a cmap format
13 subtable), so any typed character renders as the same dot. Family "PasswordCircle", the
name app/src/appearance.rs loads it under.

Usage (needs fontTools, `pip install fonttools`):
    python3 script/generate_password_font.py [output.ttf]
"""

import math
import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable

UNITS_PER_EM = 1000
ADVANCE = 600
CENTER = (300, 250)
RADIUS = 200
# Quadratic segments per circle; 8 keeps the outline round at text sizes.
SEGMENTS = 8


def circle_glyph():
    pen = TTGlyphPen(None)
    cx, cy = CENTER
    step = 2 * math.pi / SEGMENTS
    # Off-curve control radius for a quadratic arc spanning `step`.
    control = RADIUS / math.cos(step / 2)
    pen.moveTo((cx + RADIUS, cy))
    # Clockwise (decreasing angle), the TrueType convention for an outer contour.
    for i in range(SEGMENTS):
        a0 = -i * step
        mid = a0 - step / 2
        a1 = a0 - step
        pen.qCurveTo(
            (round(cx + control * math.cos(mid)), round(cy + control * math.sin(mid))),
            (round(cx + RADIUS * math.cos(a1)), round(cy + RADIUS * math.sin(a1))),
        )
    pen.closePath()
    return pen.glyph()


def build(output: Path):
    fb = FontBuilder(UNITS_PER_EM, isTTF=True)
    glyph_order = [".notdef", "circle"]
    fb.setupGlyphOrder(glyph_order)
    # Placeholder map; the real one is the format 13 subtable below.
    fb.setupCharacterMap({0x25CF: "circle"})
    fb.setupGlyf({".notdef": TTGlyphPen(None).glyph(), "circle": circle_glyph()})
    fb.setupHorizontalMetrics({".notdef": (ADVANCE, 0), "circle": (ADVANCE, CENTER[0] - RADIUS)})
    fb.setupHorizontalHeader(ascent=750, descent=-250)
    fb.setupNameTable(
        {
            "familyName": "PasswordCircle",
            "styleName": "Regular",
            "uniqueFontIdentifier": "PasswordCircle-Regular-2.0",
            "fullName": "PasswordCircle Regular",
            "psName": "PasswordCircle-Regular",
            "version": "Version 2.0",
            "copyright": "Copyright (C) 2026 TwinCurrent ApS",
            "licenseDescription": "GNU Affero General Public License, version 3 only",
        }
    )
    fb.setupOS2(sTypoAscender=750, sTypoDescender=-250, usWinAscent=750, usWinDescent=250)
    fb.setupPost()

    # Every code point to the circle: one format 13 group over 0..0x10FFFF, for both the
    # Unicode platform and Windows' full-repertoire encoding.
    subtables = []
    for platform_id, encoding_id in ((0, 4), (3, 10)):
        sub = CmapSubtable.newSubtable(13)
        sub.platformID = platform_id
        sub.platEncID = encoding_id
        sub.language = 0
        sub.cmap = dict.fromkeys(range(0x110000), "circle")
        subtables.append(sub)
    fb.font["cmap"].tables = subtables

    fb.save(str(output))


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "app/assets/bundled/fonts/password.ttf"
    build(Path(sys.argv[1]) if len(sys.argv) > 1 else default)
