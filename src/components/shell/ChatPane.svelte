<script>
    // The conversation pane: header, pins drawer, message list host, wallpaper controls
    // and the composer, each rendering from its store.
    import { shellPanes, shellReveals, reveal } from '../lib/shell.svelte.js';
    import { chatPaneHandlers } from '../lib/chatpane.svelte.js';
    import { pinsState } from '../lib/pins.svelte.js';
    import { wallpaperState } from '../lib/wallpaper.svelte.js';
    import ChatHeader from '../chat/ChatHeader.svelte';
    import PinsDrawer from '../chat/PinsDrawer.svelte';
    import WallpaperLayer from '../chat/WallpaperLayer.svelte';
    import WallpaperPreviewBar from '../chat/WallpaperPreviewBar.svelte';
    import ComposerBox from '../composer/ComposerBox.svelte';
    const panes = shellPanes();
    const reveals = shellReveals();
    const pins = pinsState();
    const wp = wallpaperState();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="chat" class="chat" style:display={panes.chat ? null : 'none'} class:pins-focus={pins.open} use:reveal={['chat', reveals.chat]} onclick={(e) => chatPaneHandlers().click?.(e)}
     data-wallpaper={wp.image ? 'true' : undefined} data-wallpaper-previewing={wp.previewing ? 'true' : undefined}>
    <WallpaperLayer />
    <ChatHeader />
    <PinsDrawer />
    <div id="msg-top-fade" class="fadeout-top-msgs" style="top: 60px;"></div>
    <div id="chat-messages" class="chat-messages"></div>
    <WallpaperPreviewBar />
    <ComposerBox />
</div>
