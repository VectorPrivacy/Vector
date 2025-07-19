# Purpose

Vector was born out of a feeling of, for the lack of a better word; "necessity".

The messengers with all the cool features, have stark downsides: opt-in proprietary encryption (Telegram), no encryption at all (Discord), or encryption added, almost seemingly through pity, and most certainly with backdoors, to apps created by the world's largest and most anti-human tech conglomerates (Meta's WhatsApp).

The messengers with the most sovereign, decentralised, E2E Encrypted philosophies: lack incredibly basic features, or have such an archaic and illegible User Experience, that the modern user of this century feels as if they've returned to the Stone Age; flooded with a tangled-web of bugs, a slowness familiar only to P2P software, and governance issues.

In addition to security and privacy in communication, modern software has hit a brick wall: you now need half a gig of RAM to open a simple messaging app, and the React framework plagues the entire web.

Modern software wastes Modern compute, because developers have gotten lazy, and project managers have gotten greedy.

**Vector; possibly naively, but surely bravely, aims to fill this gap.**

Powered by Passion, Built on [Nostr](https://nostr.com/).

---

# Compiling

> *The following process was graciously written by [PalmTree](https://primal.net/p/npub1e3zglze5g2mq894pfw42kw8uwmyd8uc6m8mupctjfkfplgddglds4v7wja), give him a follow!*

### Install Dependencies

Rust Stable and Tauri must be installed along with other dependencies. The easiest way to do that is to follow this guide:    
<https://v1.tauri.app/v1/guides/getting-started/prerequisites>  

### Compiling Vector for the First Time

Assuming you want Vector in an `apps` folder (adjust as necessary):  

```
cd ~/apps && git pull https://github.com/VectorPrivacy/Vector
```

```
cd ~/apps/Vector && yarn add -D @tauri-apps/cli && yarn install
```

```
npm run build
```

### Upgrading Vector

Check for updates:  

```
cd ~/apps/Vector && git pull
```

Compiling is only necessary if files were updated when running the previous command:

```
npm run build
```

### Vector Executables

The compiled Vector app can be found in the release folder located here:  

```
cd ~/apps/Vector/src-tauri/target/release/
```

---

# Bare Builds

Vector supports "bare builds" - a minimal compilation mode that excludes optional features for enhanced security and performance, while not the recommended experience for most users, Vector bare builds are hardened, faster, and lighter; at the expense of more "glamorous" and complex features.

### Why Use Bare Builds?

- **Reduced Attack Surface**: Fewer dependencies & less code means fewer potential vulnerabilities.
- **Resource Efficiency**: Lower memory and CPU usage, with faster boot time.
- **True Minimalism**: A powerful app, with only the core necessities.

### Building Vector Bare

```bash
# Development bare build
npm run dev:bare

# Production bare build
npm run build:bare
```

### What's Excluded?

Currently, bare builds exclude:
- Vector Voice AI (Whisper and its GPU ML dependencies, like Vulkan).

### Standard vs Bare

- **Standard Build**: Full suite of features, maximum utility and range of function.
- **Bare Build**: Core functionality only, maximum security and efficiency.

The bare build is perfect for users who prioritize security, privacy, and performance over additional features like Local AI and flashy utility features.