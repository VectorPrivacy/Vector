### The language itself, as the Language setting lists it.

# Your language's name in your language, like "Español" or "Deutsch".
language-name = English (UK)

# Your language's name in English, like "Spanish" or "German", for someone looking for it in a
# list they can't read.
language-name-english = English (UK)

## Settings > Appearance > Language: every language, built in or to download

# Under English (UK), which is part of Vector rather than a download.
language-built-in = Built in
# Under a language: its name in English, how much of Vector it translates, and its download
# size. $name is the English name, like "Spanish"; $coverage is a whole percentage; $size is
# like "41 KB".
language-detail = { $name } · { $coverage }% · { $size }
# Under a language already downloaded. $name is its English name; $coverage a whole percentage.
language-detail-installed = { $name } · { $coverage }%
# The same, for a language whose name English writes the same way. $coverage is a whole
# percentage; $size is like "41 KB".
language-detail-unnamed = { $coverage }% · { $size }
# Under a downloaded language whose name English writes the same way. $coverage is a whole
# percentage.
language-detail-coverage = { $coverage }%
# While the list of languages to download is fetched.
language-checking = Looking for languages…
# The list couldn't be fetched. $error says why.
language-check-failed = Couldn’t load the language list: { $error }
language-retry = Try Again
# While a language downloads. $percent is a whole percentage.
language-downloading = Downloading… { $percent }%
# A download failed. $error says why.
language-download-failed = Couldn’t download it: { $error }
# Opens the guide to translating Vector.
language-help-translate = Help translate Vector
# The language list's server refused. $status is its HTTP status, such as 404.
language-index-status = The language list answered { $status }
language-index-invalid = The language list couldn’t be read
language-index-unsigned = The language list isn’t signed by Vector
language-pack-mismatch = The download didn’t match the language list
language-pack-unsaved = The language couldn’t be saved on this device
# The repository translates the language, but no download of it is listed yet.
language-not-listed = It isn’t ready to download yet

## The welcome screen, on first launch, when the system's language can be downloaded

# $name is the language's name in itself, like "Español".
language-offer = Vector is available in <b>{ $name }</b>. Switch?
language-offer-switch = Switch
language-offer-dismiss = No Thanks
