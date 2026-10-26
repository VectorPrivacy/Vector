<script>
    // The list pane. Widescreen: the sync line, the network's notice, the two New buttons,
    // the community head and the list island's host. Slim (the Centre Console): a header
    // over the community rail and the pane beside it, which is the filtered list or the
    // community the rail picked.
    import { shellPanes, shellState, shellHandlers, shellScreens, shellReveals, reveal, bindShellEl, syncLineState } from '../lib/shell.svelte.js';
    import { consoleState } from '../lib/console.svelte.js';
    import { paneState } from '../lib/signals.svelte.js';
    import ConsoleHeader from './ConsoleHeader.svelte';
    import ConsoleRail from './ConsoleRail.svelte';
    import ConsoleTabs from './ConsoleTabs.svelte';
    import Chatlist from '../chatlist/Chatlist.svelte';
    import CommunityHead from '../chatlist/CommunityHead.svelte';
    import CommunityBanner from '../chatlist/CommunityBanner.svelte';
    import NetworkNotice from './NetworkNotice.svelte';
    import { loginState } from '../lib/login.svelte.js';
    import { settler, railPull } from '../lib/railmotion.js';
    import { untrack } from 'svelte';
    const panes = shellPanes();
    const login = loginState();
    const shell = shellState();
    const cc = consoleState();
    const pane = paneState();
    const screens = shellScreens();
    // Before sign-in neither layout is up, and the console waits for one.
    const slim = $derived(cc.active);
    const searching = $derived(slim && !!cc.query.trim());
    const reveals = shellReveals();
    const sync = syncLineState();
    const bindList = bindShellEl('chatList');
    let listEl = $state(null);

    // The scroller's end fades track the scroll that is left in each direction, so a
    // row you have reached is never dimmed. Rows come and go, so the content's height
    // is watched as well as the box's.
    const FADE_MAX = 24;
    function listFade(node) {
        let depth = -1, top = -1;
        const sync = () => {
            const below = node.scrollHeight - node.clientHeight - node.scrollTop;
            const d = Math.round(Math.max(0, Math.min(FADE_MAX, below)));
            const t = Math.round(Math.max(0, Math.min(FADE_MAX, node.scrollTop)));
            if (d === depth && t === top) return;
            depth = d; top = t;
            node.style.setProperty('--ws-list-fade', d + 'px');
            node.style.setProperty('--ws-list-fade-top', t + 'px');
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
    const bindNewChat = bindShellEl('newChat');
    const bindChats = bindShellEl('chats');

    // The slim rail slides rather than snaps: `railP` is how far out it is, eased toward
    // the stored state, or held to a finger mid-pull. Before the layout is up it just jumps.
    // A change of state always eases there, so nothing can leave it stranded part-way.
    const RAIL_W = 68;
    let railP = $state(1);
    const settle = settler((v) => { railP = v; });
    $effect(() => {
        const target = cc.railOpen ? 1 : 0;
        const live = cc.active;
        untrack(() => (live ? settle.to(railP, target) : (railP = target)));
    });
    const pull = {
        width: () => RAIL_W,
        begin: () => { settle.stop(); return railP; },
        move: (p) => { railP = p; },
        end: (open) => {
            if (open !== cc.railOpen) shellHandlers().consoleSetRail?.(open);
            else settle.to(railP, open ? 1 : 0);
        },
    };
    // The rail moves by its bare strip and the Messages door: a tile's drag is its own.
    const railGrip = { ...pull, grabs: (t) => !t.closest('.ws-rail-item, .ws-rail-folder, .cc-rail-add, .cc-rail-fold') };
</script>

<div id="chats" class="chats" style:display={panes.chats ? null : 'none'} use:bindChats use:reveal={['chats', reveals.chats]}>
    {#if slim}<ConsoleHeader />{/if}
    <!-- A sync that starts under the login form waits for the list to be on screen. -->
    <div id="sync-line" class="sync-line" class:active={sync.active && !login.shown} class:fade-out={sync.fadeOut} class:progress={sync.progress !== null}
         style:--sync-progress={sync.progress !== null ? sync.progress : null}></div>
    <NetworkNotice />
    {#if shell.ws}
        <div id="chat-new-actions" style="display: flex; flex-direction: row; margin: 0 15px 15px 15px;">
            <button id="new-chat-btn" class="new-chat-btn btn" style="width: 50%; margin-right: 5px;" style:display={shell.newChatButtons ? null : 'none'}
                    use:bindNewChat use:reveal={['newChat', reveals.newChat]} onclick={() => shellHandlers().openNewChat?.()}>
                <span class="icon icon-new-msg"></span>
                <span style="width: 100%">New Chat</span>
            </button>
            <button class="new-chat-btn btn" style="width: 50%; margin-left: 5px; margin-right: 10px;" style:display={shell.newChatButtons ? null : 'none'}
                    use:reveal={['newChat', reveals.newChat]} onclick={() => shellHandlers().openCreateGroup?.()}>
                <span class="icon icon-chat-circle"></span>
                <span style="width: 100%">Group Chat</span>
            </button>
        </div>
    {/if}
    <!-- Widescreen lays these wrappers out of the way (display: contents). -->
    <div class="cc-body" style:--cc-rail-p={slim ? railP : null}>
        {#if slim}
            <!-- Folded, it's still here at no width, so a pull can bring it out. -->
            <div class="cc-rail-slot" inert={railP === 0} use:railPull={railGrip}><ConsoleRail /></div>
            <!-- Folded, a swipe in from the screen's edge pulls it out as the tab does. -->
            <div class="cc-rail-edge" inert={railP === 1} use:railPull={pull}></div>
        {/if}
        <div class="cc-pane">
            {#if (slim || shell.ws) && !pane.communityId && !searching && screens.chatlist}
                <div class="cc-tabs-row">
                    {#if slim}
                        <!-- The button is the grab area, wider and taller than the tab it draws,
                             which tucks into the rail's edge as it comes out. -->
                        <button class="cc-rail-pull" title="Show communities" aria-label="Show communities" inert={railP === 1}
                                use:railPull={pull} onclick={() => shellHandlers().consoleSetRail?.(true)}>
                            <span class="cc-rail-pull-tab"><span class="icon icon-chevron-right"></span></span>
                        </button>
                    {/if}
                    <ConsoleTabs h={screens.chatlist.h} snapshot={screens.chatlist.snapshot} onPick={() => listEl?.scrollTo({ top: 0 })} />
                </div>
            {/if}
            <!-- A community's header, hosted outside the scroller so it cannot ride the
                 list's overscroll. Empty (and zero-height) otherwise. -->
            <div id="ws-community-head">
                {#if !searching}
                    {#if screens.communityHead}
                        <CommunityHead h={screens.communityHead.h} onBack={slim && !cc.railOpen ? () => shellHandlers().consoleBack?.() : null} />
                    {/if}
                    {#if screens.communityBanner}<CommunityBanner h={screens.communityBanner.h} />{/if}
                {/if}
            </div>
            <div id="chat-list" bind:this={listEl} use:bindList use:listFade use:reveal={['chatList', reveals.chatList]}>
                {#if screens.chatlist}<Chatlist h={screens.chatlist.h} snapshot={screens.chatlist.snapshot} />{/if}
            </div>
        </div>
    </div>
</div>
