# Attributions

This document contains attributions for third-party assets used in Vector.

---

## Icons

### Robot Icon (bot.svg)

- **Source**: [SVG Repo](https://www.svgrepo.com/svg/521818/robot)
- **License**: [CC Attribution License](https://creativecommons.org/licenses/by/4.0/)
- **Author**: [Konstantin Filatov](https://www.svgrepo.com/author/Konstantin%20Filatov/)
- **Usage**: Used as the bot indicator icon in the chat list to identify bot accounts
- **Modifications**: The icon form is not modified; however, it may be displayed in different colors adherent to the app's theme settings

The CC Attribution License requires attribution to the original author, which we provide here in compliance with the license terms.

---

## Libraries

### Marked.js

- **Source**: [Marked.js GitHub](https://github.com/markedjs/marked)
- **Version**: v16.4.1
- **License**: [MIT License](https://github.com/markedjs/marked/blob/master/LICENSE.md)
- **Author**: Christopher Jeffrey and contributors
- **Usage**: Used as the markdown parser for rendering formatted text in chat messages
- **File**: `src/js/marked.min.js`

Marked.js is a fast, low-level markdown compiler that follows the CommonMark specification. It provides Discord-like markdown rendering with support for bold, italic, strikethrough, code blocks, links, images, lists, and more.

### Highlight.js

- **Source**: [Highlight.js GitHub](https://github.com/highlightjs/highlight.js)
- **Version**: v11.9.0
- **License**: [BSD 3-Clause License](https://github.com/highlightjs/highlight.js/blob/main/LICENSE)
- **Copyright**: (c) 2006-2023 Highlight.js contributors
- **Usage**: Used for syntax highlighting in code blocks within chat messages
- **File**: `src/js/highlight.min.js`

Highlight.js provides automatic language detection and syntax highlighting for over 190 programming languages. It integrates seamlessly with Marked.js to provide beautiful, readable code blocks in chat messages.

### DOMPurify

- **Source**: [DOMPurify GitHub](https://github.com/cure53/DOMPurify)
- **Version**: v3.0.6
- **License**: [Apache License 2.0](https://github.com/cure53/DOMPurify/blob/main/LICENSE)
- **Author**: Cure53 and DOMPurify contributors
- **Usage**: Sanitizes rendered Markdown HTML to prevent XSS in chat messages
- **File**: `src/js/dompurify.min.js`

DOMPurify provides robust, battle-tested HTML sanitization. It ensures that rendered Markdown content cannot execute malicious scripts or inject unsafe markup, giving the chat interface strong protection against XSS attacks.

### Twemoji

- **Source**: [Twemoji GitHub](https://github.com/twitter/twemoji)
- **Version**: v14.0.2
- **License**: [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/) (graphics), [MIT License](https://github.com/twitter/twemoji/blob/master/LICENSE-CODE) (code)
- **Author**: Twitter, Inc. and contributors
- **Usage**: Used for rendering emoji as SVG images in chat messages and throughout the app
- **Files**: `src/js/twemoji.min.js`, `src/twemoji/svg/*.svg`

Twemoji provides a consistent, high-quality emoji experience across all platforms. The library converts Unicode emoji characters into Twitter's emoji graphics, ensuring that emoji look the same regardless of the user's operating system or device.

---

## Sounds

### Sonar Ping (notif-sonar.mp3)

- **Source**: [Freesound](https://freesound.org/people/shinephoenixstormcrow/sounds/337050/)
- **License**: [CC Attribution 3.0](https://creativecommons.org/licenses/by/3.0/)
- **Author**: [shinephoenixstormcrow](https://freesound.org/people/shinephoenixstormcrow/) (modified from [unfa](https://freesound.org/people/unfa/)'s original)
- **Usage**: Used as the "Sonar" notification sound option
- **File**: `src-tauri/resources/sounds/notif-sonar.mp3`

### Techno Ping (notif-techno.mp3)

- **Source**: [Freesound](https://freesound.org/people/Alexhanj/sounds/528730/)
- **License**: [CC0 1.0 (Public Domain)](https://creativecommons.org/publicdomain/zero/1.0/)
- **Author**: [Alexhanj](https://freesound.org/people/Alexhanj/)
- **Usage**: Used as the "Techno" notification sound option
- **File**: `src-tauri/resources/sounds/notif-techno.mp3`

### Synth Ping (notif-synth.mp3)

- **Source**: [Freesound](https://freesound.org/people/SUBQUiRE/sounds/833599/)
- **License**: [CC Attribution 4.0](https://creativecommons.org/licenses/by/4.0/)
- **Author**: [SUBQUiRE](https://freesound.org/people/SUBQUiRE/)
- **Usage**: Used as the "Synth" notification sound option
- **File**: `src-tauri/resources/sounds/notif-synth.mp3`

---

## Additional Notes

Vector strives to properly attribute all third-party assets and respect the licenses under which they are provided. If you believe any attribution is missing or incorrect, please [open an issue](https://github.com/VectorPrivacy/Vector/issues) or contact the development team.