<script>
    // The conversation pane: header, a DM request's bar, pins drawer, message list host,
    // wallpaper controls and the composer, each rendering from its store.
    import { shellPanes, shellReveals, shellHandlers, reveal, bindShellEl } from '../lib/shell.svelte.js';
    import { chatPaneHandlers } from '../lib/chatpane.svelte.js';
    import { pinsState } from '../lib/pins.svelte.js';
    import { wallpaperState, wallpaperShown } from '../lib/wallpaper.svelte.js';
    import ChatHeader from '../chat/ChatHeader.svelte';
    import PinsDrawer from '../chat/PinsDrawer.svelte';
    import VoiceStage from '../calls/VoiceStage.svelte';
    import WallpaperLayer from '../chat/WallpaperLayer.svelte';
    import WallpaperPreviewBar from '../chat/WallpaperPreviewBar.svelte';
    import ComposerBox from '../composer/ComposerBox.svelte';
    import RequestBar from '../chat/RequestBar.svelte';
    import { requestState } from '../lib/request.svelte.js';
    import { consoleState } from '../lib/console.svelte.js';
    import { railPull } from '../lib/railmotion.js';
    import { drawerPull } from '../lib/drawer.js';
    const req = requestState();
    const cc = consoleState();

    // The drawer comes out under a sideways pull from the conversation or its header. What
    // scrolls sideways itself, or turns under a drag, keeps the gesture.
    function sideways(el) {
        for (; el && !el.classList.contains('chat-messages'); el = el.parentElement) {
            if (el.scrollWidth > el.clientWidth && /auto|scroll/.test(getComputedStyle(el).overflowX)) return true;
        }
        return false;
    }
    const drawerGrip = drawerPull({
        live: () => cc.layer,
        grabs: (t) => !!t.closest('.chat-messages, .chat-header') && !t.closest('.model3d, .audio-waveform') && !sideways(t),
        end: (open) => shellHandlers().consoleDrawer?.(open),
    });
    const panes = shellPanes();
    const reveals = shellReveals();
    const bindChat = bindShellEl('chat');
    const pins = pinsState();
    const wp = wallpaperState();

    // The typing strip's shadow tracks the scroll left below, as the list rail's fade does:
    // none while pinned, full once a fade's height of history sits under it. Its width
    // stops at the scrollbar, which never fades.
    const TYPING_FADE = 15;
    function typingFade(node) {
        const pane = node.parentElement;
        let fade = -1, bar = -1;
        const sync = () => {
            const below = node.scrollHeight - node.clientHeight - node.scrollTop;
            const f = Math.round(Math.max(0, Math.min(1, below / TYPING_FADE)) * 100) / 100;
            const b = node.offsetWidth - node.clientWidth;
            if (f !== fade) { fade = f; pane.style.setProperty('--typing-fade', String(f)); }
            if (b !== bar) { bar = b; pane.style.setProperty('--typing-bar', b + 'px'); }
        };
        const later = () => requestAnimationFrame(sync);
        node.addEventListener('scroll', sync, { passive: true });
        const ro = new ResizeObserver(sync);
        ro.observe(node);
        const mo = new MutationObserver(later);
        mo.observe(node, { childList: true, subtree: true });
        sync();
        return { destroy() { node.removeEventListener('scroll', sync); ro.disconnect(); mo.disconnect(); } };
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="chat" class="chat" style:display={panes.chat ? null : 'none'} class:pins-focus={pins.open} class:has-request={!!req.chatId} use:bindChat use:reveal={['chat', reveals.chat]} use:railPull={drawerGrip} onclick={(e) => chatPaneHandlers().click?.(e)}
     data-wallpaper={wallpaperShown() ? 'true' : undefined} data-wallpaper-previewing={wp.previewing ? 'true' : undefined}>
    <WallpaperLayer />
    <ChatHeader />
    <RequestBar />
    <PinsDrawer />
    <VoiceStage />
    <div id="msg-top-fade" class="fadeout-top-msgs" style="top: 60px;"></div>
    <div id="chat-messages" class="chat-messages" use:typingFade></div>
    <WallpaperPreviewBar />
    <ComposerBox />
</div>
