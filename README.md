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
cd ~/apps && git pull https://github.com/JSKitty/Vector
```

```
cd ~/apps/Vector && yarn add -D @tauri-apps/cli && yarn install
```

```
npm run tauri build
```

### Upgrading Vector

Check for updates:  

```
cd ~/apps/Vector && git pull
```

Compiling is only necessary if files were updated when running the previous command:

```
npm run tauri build
```

### Vector Executables

The compiled Vector app can be found in the release folder located here:  

```
cd ~/apps/Vector/src-tauri/target/release/
```
