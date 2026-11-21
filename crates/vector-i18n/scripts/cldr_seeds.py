#!/usr/bin/env python3
"""Writes `seeds/<tag>.json`: each language's starting `locale.json`, from CLDR's JSON packages,
so `vector-i18n new <tag>` hands a translator dates, lists and numbers already right.

    npm pack cldr-core@45 cldr-dates-modern@45 cldr-numbers-modern@45 cldr-misc-modern@45
    # untar each into <cldr>/<package name>, then:
    python3 -I scripts/cldr_seeds.py <cldr>

Only what differs from the built-in en-GB data (`LocaleData::default()`, mirrored in `BUILT_IN`)
is written, and en-GB itself must come out empty, which checks the mapping."""

import json
import re
import sys
from pathlib import Path

VERSION = "45.0.0"
# Vector's tag, then CLDR's locale where they differ: `pt` is European Portuguese, pt-BR has its own.
TAGS = {
    "en-GB": "en-GB", "ar": "ar", "bg": "bg", "ca": "ca", "cs": "cs", "da": "da", "de": "de",
    "el": "el", "es": "es", "es-419": "es-419", "et": "et", "fa": "fa", "fi": "fi", "fr": "fr",
    "he": "he", "hr": "hr", "hu": "hu", "id": "id", "it": "it", "ja": "ja", "ko": "ko", "lt": "lt",
    "lv": "lv", "nb": "nb", "nl": "nl", "pl": "pl", "pt": "pt-PT", "pt-BR": "pt", "ro": "ro",
    "ru": "ru", "sk": "sk", "sl": "sl", "sr": "sr", "sv": "sv", "th": "th", "tr": "tr", "uk": "uk",
    "vi": "vi", "zh-Hans": "zh-Hans", "zh-Hant": "zh-Hant",
}
DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]
LATIN = "0123456789"
BUILT_IN = {
    "decimal": ".",
    "group": ",",
    "list_sep": ", ",
    "list_last": " and ",
    "list_pair": " and ",
    "digits": LATIN,
    "dates": {
        "months": ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
        "months_short": ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sept", "Oct", "Nov", "Dec"],
        "weekdays": ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
        "weekdays_short": ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
        "am_pm": ["am", "pm"],
        "patterns": ["dd/MM/y", "dd/MM/y", "d MMM y", "d MMMM y", "EEEE d MMMM y", "d MMM", "EEE d MMM", "EEE, d MMM y", "EEE"],
        "times": ["h:mm\u202fa", "h:mm:ss\u202fa", "HH:mm", "HH:mm:ss"],
        "date_time": "{0}, {1}",
        "hour12": False,
        "week_start": 1,
    },
}


def load(root, package, *path):
    with open(root / f"{package}-{VERSION}" / Path(*path), encoding="utf-8") as f:
        return json.load(f)


def outside_quotes(pattern, fn):
    """Applies `fn` to the pattern letters only: 'quoted' text is literal."""
    parts = re.split(r"('(?:[^']|'')*')", pattern)
    return "".join(p if p.startswith("'") else fn(p) for p in parts)


def ours(pattern):
    # Flexible day periods (`B`, `b`) read as AM/PM, and the era (`G`) goes: the formatter has
    # no tables for either.
    return outside_quotes(pattern, lambda p: re.sub(r" ?G+ ?", " ", re.sub(r"[Bb]+", "a", p))).strip()


def padded(pattern):
    return outside_quotes(pattern, lambda p: re.sub(r"(?<![dM])([dM])(?![dM])", r"\1\1", p))


def between(pattern):
    if not (pattern.startswith("{0}") and pattern.endswith("{1}")):
        raise ValueError(f"list pattern {pattern!r}")
    return pattern[3:-3]


def region_of(locale, likely):
    for part in locale.split("-")[1:]:
        if len(part) == 2 and part.isupper() or part.isdigit():
            return part
    full = likely.get(locale) or likely.get(locale.split("-")[0])
    return full.split("-")[-1]


def seed(root, locale, core):
    likely, time_data, first_day = core
    g = load(root, "cldr-dates-modern", "main", locale, "ca-gregorian.json")["main"][locale]["dates"]["calendars"]["gregorian"]
    n = load(root, "cldr-numbers-modern", "main", locale, "numbers.json")["main"][locale]["numbers"]
    lists = load(root, "cldr-misc-modern", "main", locale, "listPatterns.json")["main"][locale]["listPatterns"]["listPattern-type-standard"]
    af = g["dateTimeFormats"]["availableFormats"]
    region = region_of(locale, likely)
    system = n["defaultNumberingSystem"]
    symbols = n[f"symbols-numberSystem-{system}"]
    digits = LATIN
    if system != "latn":
        digits = load(root, "cldr-core", "supplemental", "numberingSystems.json")["supplemental"]["numberingSystems"][system]["_digits"]
    clock = (time_data.get(region) or time_data["001"])["_preferred"]
    date_time = g["dateTimeFormats"]["medium"]
    date_time = date_time.replace("{0}", "\0").replace("{1}", "{0}").replace("\0", "{1}")
    return {
        "decimal": symbols["decimal"],
        "group": symbols["group"],
        "list_sep": between(lists["middle"]),
        "list_last": between(lists["end"]),
        "list_pair": between(lists["2"]),
        "digits": digits,
        "dates": {
            "months": [g["months"]["format"]["wide"][str(i)] for i in range(1, 13)],
            "months_short": [g["months"]["format"]["abbreviated"][str(i)] for i in range(1, 13)],
            "weekdays": [g["days"]["format"]["wide"][d] for d in DAYS],
            "weekdays_short": [g["days"]["format"]["abbreviated"][d] for d in DAYS],
            "am_pm": [g["dayPeriods"]["format"]["abbreviated"]["am"], g["dayPeriods"]["format"]["abbreviated"]["pm"]],
            "patterns": [ours(p) for p in [
                af["yMd"],
                padded(af["yMd"]),
                af["yMMMd"],
                g["dateFormats"]["long"],
                g["dateFormats"]["full"],
                af["MMMd"],
                af["MMMEd"],
                af["yMMMEd"],
                af["E"],
            ]],
            "times": [ours(af[k]) for k in ["hm", "hms", "Hm", "Hms"]],
            "date_time": date_time,
            "hour12": clock in ("h", "K"),
            "week_start": DAYS.index((first_day.get(region) or first_day["001"])),
        },
    }


def diff(value, base):
    if isinstance(value, dict):
        out = {k: d for k, v in value.items() if (d := diff(v, base.get(k))) is not None}
        return out or None
    return None if value == base else value


def main():
    root = Path(sys.argv[1])
    core = (
        load(root, "cldr-core", "supplemental", "likelySubtags.json")["supplemental"]["likelySubtags"],
        load(root, "cldr-core", "supplemental", "timeData.json")["supplemental"]["timeData"],
        load(root, "cldr-core", "supplemental", "weekData.json")["supplemental"]["weekData"]["firstDay"],
    )
    out = Path(__file__).resolve().parent.parent / "seeds"
    out.mkdir(exist_ok=True)
    for tag, locale in TAGS.items():
        d = diff(seed(root, locale, core), BUILT_IN)
        if tag == "en-GB":
            # The built-in data spells `E` as `EEE` and writes `Wed 18 Nov` without CLDR's comma.
            same = lambda p: re.sub(r"(?<![Ec])(E{1,3}|c{1,3})(?![Ec])", "EEE", p).replace("EEE, d MMM", "EEE d MMM")
            patterns = (d or {}).get("dates", {}).pop("patterns", BUILT_IN["dates"]["patterns"])
            if list(map(same, patterns)) != list(map(same, BUILT_IN["dates"]["patterns"])) or diff(d or {}, {"dates": {}}):
                sys.exit(f"en-GB differs from the built-in data: {d} {patterns}")
            continue
        text = json.dumps(d, ensure_ascii=False, indent=2)
        # A list of names reads best on one line.
        text = re.sub(r"\[\n\s+([^\[\]{}]*?)\n\s+\]", lambda m: "[" + re.sub(r",\n\s+", ", ", m.group(1)) + "]", text)
        (out / f"{tag}.json").write_text(text + "\n", encoding="utf-8")
    print(f"{len(TAGS) - 1} seeds in {out}")


if __name__ == "__main__":
    main()
