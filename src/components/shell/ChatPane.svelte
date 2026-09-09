<script>
    // The conversation pane: header, pins drawer, message list host, wallpaper controls
    // and the composer, each rendering from its store.
    import { shellPanes } from '../lib/shell.svelte.js';
    import { pinsState } from '../lib/pins.svelte.js';
    import { wallpaperState } from '../lib/wallpaper.svelte.js';
    import ChatHeader from '../chat/ChatHeader.svelte';
    import PinsDrawer from '../chat/PinsDrawer.svelte';
    import WallpaperLayer from '../chat/WallpaperLayer.svelte';
    import WallpaperPreviewBar from '../chat/WallpaperPreviewBar.svelte';
    import ComposerBox from '../composer/ComposerBox.svelte';
    const panes = shellPanes();
    const pins = pinsState();
    const wp = wallpaperState();
</script>

<div id="chat" class="chat" style:display={panes.chat ? null : 'none'} class:pins-focus={pins.open}
     data-wallpaper={wp.image ? 'true' : undefined} data-wallpaper-previewing={wp.previewing ? 'true' : undefined}>
    <WallpaperLayer />
    <ChatHeader />
    <PinsDrawer />
    <div id="msg-top-fade" class="fadeout-top-msgs" style="top: 60px;"></div>
    <div id="chat-messages" class="chat-messages"></div>
    <WallpaperPreviewBar />
    <ComposerBox />
</div>
