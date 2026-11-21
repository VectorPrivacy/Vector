#!/usr/bin/env python3
"""Writes `locales/<tag>/emoji.tsv`: the language's emoji names and search words from CLDR, which
its pack carries so the emoji picker names and finds emoji in that language.

    npm pack cldr-annotations-full@45 cldr-annotations-derived-full@45
    # untar each into <cldr>/<package name>, then:
    python3 -I scripts/cldr_emoji.py <cldr> ja zh-Hans …

Skin-tone variants are left out: the picker searches the base emoji. A regional locale (pt-PT)
lists only where it differs, so its parent's names come first."""

import json
import sys
from pathlib import Path

VERSION = "45.0.0"
# Vector's tag, then CLDR's locale where they differ.
CLDR = {"pt": "pt-PT", "pt-BR": "pt", "zh-Hans": "zh"}
TONES = {chr(c) for c in range(0x1F3FB, 0x1F400)}


def annotations(root, locale):
    out = {}
    for package, key, sub in [
        ("cldr-annotations-full", "annotations", "annotations"),
        ("cldr-annotations-derived-full", "annotationsDerived", "annotationsDerived"),
    ]:
        path = root / f"{package}-{VERSION}" / sub / locale / "annotations.json"
        if path.exists():
            with open(path, encoding="utf-8") as f:
                out.update(json.load(f)[key]["annotations"])
    return out


def main():
    root = Path(sys.argv[1])
    locales = Path(__file__).resolve().parent.parent / "locales"
    for tag in sys.argv[2:]:
        locale = CLDR.get(tag, tag)
        merged = {}
        parts = locale.split("-")
        for i in range(1, len(parts) + 1):
            for emoji, a in annotations(root, "-".join(parts[:i])).items():
                merged.setdefault(emoji, {}).update(a)
        if not merged:
            sys.exit(f"{tag}: CLDR has no annotations for {locale}")
        lines = []
        for emoji, a in merged.items():
            name = (a.get("tts") or [""])[0].strip()
            if not name or TONES & set(emoji) or "\t" in name:
                continue
            words = [w.strip() for w in a.get("default", []) if w.strip() and w.strip() != name and "|" not in w]
            lines.append(f"{emoji}\t{name}\t{'|'.join(words)}")
        folder = locales / tag
        if not folder.is_dir():
            sys.exit(f"{tag}: no locales/{tag} yet; `vector-i18n new {tag}` makes it")
        (folder / "emoji.tsv").write_text("\n".join(sorted(lines)) + "\n", encoding="utf-8")
        print(f"{tag}: {len(lines)} emoji")


if __name__ == "__main__":
    main()
