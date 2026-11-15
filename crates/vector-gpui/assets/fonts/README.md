# Fonts

All three families are under the SIL Open Font License 1.1 (https://openfontlicense.org).

- **Rubik** (Copyright 2015 The Rubik Project Authors): static instances cut from the variable
  `src/fonts/Rubik.woff2` and `Rubik-Italic.woff2` with fontTools, at the weights the stylesheets use.
- **Inter** (Copyright 2016 The Inter Project Authors): the stylesheets name it for chrome text.
- **Vector Slot**: cut from Rubik-Regular with fontTools, holding only a blank em space (U+2003) and
  four-per-em space (U+2005) on Rubik's metrics, plus `m`, which the macOS text system requires. The
  composer shapes an emoji's slot in it, since Rubik lacks both spaces.
