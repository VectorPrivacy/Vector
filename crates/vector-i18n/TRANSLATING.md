# Translating Vector

Vector's own text (buttons, menus, settings, errors) lives in plain text files written in
[Fluent](https://projectfluent.org/fluent/guide/), the format Firefox uses. People's messages are
never translated, only the app around them.

British English in `locales/en-GB/` is the source and the only language built into the app.
Every other language is a folder beside it that mirrors en-GB file for file, and reaches people
as a small download.

## Start a language

From `crates/vector-i18n`:

```bash
cargo run --bin vector-i18n -- new es
```

That creates `locales/es/` with every file and nothing translated. Name the folder with the
language's tag: `es`, `de`, `pt-BR`, `zh-Hans`. A regional language falls back to its parent
(`pt-BR` to `pt`), then to English, one message at a time, so a language that is only half done
still works.

Translate `language.ftl` first: `language-name` is your language's name in itself ("Español",
"Deutsch"), and `language-name-english` its name in English ("Spanish", "German"). The Language
setting lists both.

An English variant (`en-US`, `en-AU`) translates only what it spells or says differently: every
message it leaves out is en-GB's, and counts as done.

## What a message looks like

```ftl
# The pill above the chat list that jumps to the first unread message.
unread-jump = { $count ->
    [one] { $count } unread message
   *[other] { $count } unread messages
}
```

- **Keep the id** (`unread-jump`), translate what follows `=`. Lines starting with `#` are notes
  for you; they don't need translating, and they are left out of the download.
- **Attributes** (`.tip`, `.label`, `.desc`, `.ask`) belong to the message above them: a tooltip,
  what a screen reader says, a description. Translate each one. `.keys` holds words people might
  search Settings for: write the words someone would type in your language.
- **Values** like `{ $name }` or `{ $count }` are filled in by the app. Put each wherever your
  grammar needs it; the note above the message says what each is. You may leave one out where
  your sentence doesn't need it (a `[one]` form that says "a message"), but never add one that
  English doesn't give.
- **Plurals** use your language's own forms: `zero`, `one`, `two`, `few`, `many` and `other`, as
  [CLDR lists them](https://www.unicode.org/cldr/charts/latest/supplemental/language_plural_rules.html).
  Use as many as your language has, whatever English uses. The `*` marks the fallback form, and
  every choice needs one.
- **Tags** mark part of a sentence. `<b>…</b>` is bold (usually a name), `<br>` is a line break,
  and a named tag such as `<back>…</back>` is a link the app makes clickable. Keep the same tags
  English has, around whichever words fit your sentence.
- **Names stay as they are**: Vector, Nostr, npub, nsec, PIVX, Tor, I2P, Blossom.
- **Right-to-left languages** (Arabic, Hebrew, Persian, Urdu…) need no direction marks. A
  translated sentence reads right to left even when it opens with a Latin name or a value, and
  the app mirrors its layout to match.
- **`core.ftl`** holds what Vector's engine says: errors, network status, moderation policies.
  The app recognises each English line the engine writes and shows yours in its place, so
  translate them like any other message. A value marked "often in English" is a technical
  reason from a server or library, and stays as it came.

## Numbers and dates

Numbers, sizes, lists and dates follow your language through an optional `locale.json` in its
folder. Leave out any field English already gets right. Take the values from
[CLDR](https://cldr.unicode.org/) for your language:

```json
{
  "decimal": ",",
  "group": ".",
  "list_sep": ", ",
  "list_last": " y ",
  "list_pair": " y ",
  "dates": {
    "months": ["enero", "febrero", "…"],
    "patterns": ["d/M/y", "dd/MM/y", "d MMM y", "d 'de' MMMM 'de' y", "…"],
    "hour12": false,
    "week_start": 1
  }
}
```

`digits` is zero to nine as your language writes them, such as `"٠١٢٣٤٥٦٧٨٩"` for Arabic; leave
it out to keep `0123456789`.
`dates.patterns` holds one CLDR pattern for each of the app's nine date styles, in the order
`crates/vector-i18n/src/dates.rs` lists them.

## Check your work

```bash
cargo run --bin vector-i18n -- status       # every language's coverage
cargo run --bin vector-i18n -- status -v    # and each missing or obsolete id
```

- **Missing**: in English, not translated yet. It shows in English meanwhile.
- **Obsolete**: English no longer has it. Safe to delete.
- **Broken**: doesn't parse, uses a value English doesn't give, lacks a `*` default, or has
  different tags. It shows in English until fixed.

When the meaning of an English message changes it gets a new id, so the old translation shows as
obsolete and the new one as missing. A typo fix keeps its id, and translations stay valid.

## See it in the app

From `crates/vector-gpui`:

```bash
VECTOR_LANG_DIR=../vector-i18n/locales VECTOR_LANG=es cargo run
```

Long words and clipped labels show up quickly this way. `VECTOR_LANG=en-XA` shows English
accented and stretched by a third, which is how the app is tested for longer languages.

To try the language as people will get it, build its download:

```bash
cargo run --bin vector-i18n -- pack es        # writes es.vpack
```

and put the file in the `languages` folder inside Vector's data folder. It then shows under
Settings > Appearance > Language.

## Publishing

Once a translation merges, CI (`.github/workflows/languages.yaml`) builds every language at 10%
or more and signs an index of them with Vector's language key. The packs and the index go to the
`languages` release, the packs to Blossom too, and the index to Vector's relays. The app checks
the signature against the key it has built in, and each pack against the hash the index gives,
so any mirror is safe.

The key signs nothing but language indexes. Whoever holds it makes it once, from
`crates/vector-i18n`:

```bash
cargo run --features sign --bin vector-i18n -- keygen languages.key && gh secret set VECTOR_LANGUAGES_KEY -R VectorPrivacy/Vector < languages.key
```

That prints the public half, which goes in `SIGNER` in
`crates/vector-gpui/src/backend/langpacks.rs`. Keep `languages.key` offline as the backup.

To move to a new key, CI holds both for one run: the old one in `VECTOR_LANGUAGES_OLD_KEY` signs
an index naming the new one, and apps that see both trust the new key from then on.
