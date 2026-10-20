#!/usr/bin/env python3
"""Remove selected optional brand symbols while preserving other font coverage."""

import argparse
import hashlib
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont


SOURCE_SHA256 = "2dc316f2505a0cbfbcf6060a1b4ba85b0a2974189e30c0037cdedc436a25a4ff"
EXCLUDED_CODEPOINTS = {
    0xE817, 0xE87B, 0xE89B, 0xE8BC, 0xEC10, 0xEC1E, 0xEC1F, 0xEC20,
    0xEC21, 0xEC38, 0xEC3A, 0xEC3B, 0xEC3D, 0xEC3E, 0xEC42, 0xEC47,
    0xEC4C, 0xEC4D, 0xEC4E, 0xEC4F, 0xEC50, 0xEC51, 0xEC52, 0xEC53,
    0xEC54, 0xEC55, 0xEC56, 0xEC59, 0xEC67, 0xEC80, 0xEC81, 0xEC82,
    0xF4B8, 0xF4B9, 0xF4BA,
}


def outline(font, glyph_name):
    glyph = font["glyf"][glyph_name]
    coordinates, end_points, flags = glyph.getCoordinates(font["glyf"])
    program = glyph.program.getBytecode() if hasattr(glyph, "program") else b""
    return tuple(coordinates), tuple(end_points), bytes(flags), program


def verify(original, prepared, icons_source):
    before = original.getBestCmap()
    after = prepared.getBestCmap()
    expected = set(before) - EXCLUDED_CODEPOINTS
    if set(after) != expected:
        raise ValueError("Prepared font has unexpected character coverage")
    expected_count = len(original.getGlyphOrder()) - len(EXCLUDED_CODEPOINTS)
    if len(prepared.getGlyphOrder()) != expected_count:
        raise ValueError("Prepared font has unexpected glyph coverage")

    for codepoint in sorted(expected):
        old_name, new_name = before[codepoint], after[codepoint]
        if original["hmtx"][old_name] != prepared["hmtx"][new_name]:
            raise ValueError(f"Spacing changed for U+{codepoint:04X}")
        if outline(original, old_name) != outline(prepared, new_name):
            raise ValueError(f"Outline or hinting changed for U+{codepoint:04X}")

    for table, fields in {
        "head": ("unitsPerEm",),
        "hhea": ("ascent", "descent", "lineGap", "advanceWidthMax"),
        "OS/2": ("sTypoAscender", "sTypoDescender", "sTypoLineGap",
                 "usWinAscent", "usWinDescent"),
        "post": ("isFixedPitch", "italicAngle", "underlinePosition", "underlineThickness"),
    }.items():
        for field in fields:
            if getattr(original[table], field) != getattr(prepared[table], field):
                raise ValueError(f"Font metric changed: {table}.{field}")
    for tag in ("cvt ", "gasp"):
        if original.getTableData(tag) != prepared.getTableData(tag):
            raise ValueError(f"Font rendering table changed: {tag}")
    if prepared["post"].formatType != 3.0:
        raise ValueError("Prepared font still contains custom glyph names")

    required = {ord(char) for char in icons_source.read_text() if ord(char) >= 0xE000}
    if not required <= expected:
        raise ValueError("Prepared font is missing a built-in icon")
    original_names = {(n.nameID, n.platformID, n.platEncID, n.langID): n.toUnicode()
                      for n in original["name"].names}
    prepared_names = {(n.nameID, n.platformID, n.platEncID, n.langID): n.toUnicode()
                      for n in prepared["name"].names}
    if original_names != prepared_names:
        raise ValueError("Font identity or license metadata changed")
    return len(expected), len(required)


def prepare(source, destination, icons_source):
    if hashlib.sha256(source.read_bytes()).hexdigest() != SOURCE_SHA256:
        raise ValueError("Source font does not match the pinned upstream checksum")
    original = TTFont(source, recalcTimestamp=False)
    before = original.getBestCmap()
    if not EXCLUDED_CODEPOINTS <= set(before):
        raise ValueError("Source font does not contain the expected optional symbols")
    excluded_names = {before[cp] for cp in EXCLUDED_CODEPOINTS}
    font = TTFont(source, recalcTimestamp=False)
    options = subset.Options()
    options.glyph_names = False
    options.notdef_outline = True
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.legacy_cmap = True
    options.symbol_cmap = True
    options.drop_tables.append("PfEd")
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(glyphs=set(font.getGlyphOrder()) - excluded_names)
    subsetter.subset(font)
    destination.parent.mkdir(parents=True, exist_ok=True)
    font.save(destination)
    with TTFont(destination, recalcTimestamp=False) as prepared:
        kept, required = verify(original, prepared, icons_source)
    print(f"Verified {kept} retained characters and {required} built-in icons")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--icons-source", type=Path,
                        default=Path(__file__).resolve().parents[1] / "src/icons.rs")
    args = parser.parse_args()
    prepare(args.source, args.destination, args.icons_source)


if __name__ == "__main__":
    main()
