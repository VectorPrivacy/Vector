<script>
    // The list pane: bookmarks, the account row (profile.js fills it), the sync line,
    // the two New buttons, the widescreen community head and the list island's host.
    import { shellPanes, shellState, shellHandlers, shellScreens, shellReveals, reveal, bindShellEl, syncLineState } from '../lib/shell.svelte.js';
    import { accountState, accountHandlers } from '../lib/account.svelte.js';
    import AccountRow from './AccountRow.svelte';
    import Chatlist from '../chatlist/Chatlist.svelte';
    import CommunityHead from '../chatlist/CommunityHead.svelte';
    import { listHasRows } from '../lib/signals.svelte.js';
    import { loginState } from '../lib/login.svelte.js';
    const panes = shellPanes();
    const login = loginState();
    const shell = shellState();
    const acct = accountState();
    const screens = shellScreens();
    const reveals = shellReveals();
    const sync = syncLineState();
    const bindList = bindShellEl('chatList');
    const bindNewChat = bindShellEl('newChat');
    const bindChats = bindShellEl('chats');

    function fadeIn(node, tick) {
        const play = (t) => {
            if (!t) return;
            node.classList.add('fadein-anim');
            node.addEventListener('animationend', () => node.classList.remove('fadein-anim'), { once: true });
        };
        play(tick);
        return { update: play };
    }
</script>

<div id="chats" class="chats" style:display={panes.chats ? null : 'none'} use:bindChats use:reveal={['chats', reveals.chats]}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chat-bookmarks-btn" class="btn chat-bookmarks-btn" style="z-index: 5;" style:display={acct.bookmarks ? 'flex' : 'none'}
         use:fadeIn={acct.revealTick} onclick={() => accountHandlers().openBookmarks?.()}>
        <span class="icon icon-bookmark nav-icon"></span>
    </div>
    {#if !shell.ws}
        <AccountRow />
    {/if}
    <!-- A sync that starts under the login form waits for the list to be on screen. -->
    <div id="sync-line" class="sync-line" class:active={sync.active && !login.shown} class:fade-out={sync.fadeOut} class:progress={sync.progress !== null}
         style:--sync-progress={sync.progress !== null ? sync.progress : null}></div>
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
    <!-- Widescreen: a community's header, hosted outside the scroller so it cannot
         ride the list's overscroll. Empty (and zero-height) otherwise. -->
    <div id="ws-community-head">
        {#if screens.communityHead}<CommunityHead h={screens.communityHead.h} />{/if}
    </div>
    <div id="chat-list" use:bindList use:reveal={['chatList', reveals.chatList]}>
        {#if screens.chatlist}<Chatlist h={screens.chatlist.h} snapshot={screens.chatlist.snapshot} />{/if}
    </div>
    <div class="fadeout-bottom" style="position: fixed; bottom: 65px;" style:display={listHasRows() ? null : 'none'}></div>
</div>
