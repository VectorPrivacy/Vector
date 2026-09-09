<script>
    // The list pane: bookmarks, the account row (profile.js fills it), the sync line,
    // the two New buttons, the widescreen community head and the list island's host.
    import { shellPanes, shellState } from '../lib/shell.svelte.js';
    import { accountState, accountHandlers } from '../lib/account.svelte.js';
    import AccountRow from './AccountRow.svelte';
    const panes = shellPanes();
    const shell = shellState();
    const acct = accountState();

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

<div id="chats" class="chats" style:display={panes.chats ? null : 'none'}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chat-bookmarks-btn" class="btn chat-bookmarks-btn" style="z-index: 5;" style:display={acct.bookmarks ? 'flex' : 'none'}
         use:fadeIn={acct.revealTick} onclick={() => accountHandlers().openBookmarks?.()}>
        <span class="icon icon-bookmark nav-icon"></span>
    </div>
    {#if !shell.ws}
        <AccountRow />
    {/if}
    <div id="sync-line" class="sync-line"></div>
    <div id="chat-new-actions" style="display: flex; flex-direction: row; margin: 0 15px 15px 15px;">
        <button id="new-chat-btn" class="new-chat-btn btn" style="width: 50%; margin-right: 5px; display: none;">
            <span class="icon icon-new-msg"></span>
            <span style="width: 100%">New Chat</span>
        </button>
        <button id="create-group-btn" class="new-chat-btn btn" style="width: 50%; margin-left: 5px; margin-right: 10px; display: none;">
            <span class="icon icon-chat-circle"></span>
            <span style="width: 100%">Group Chat</span>
        </button>
    </div>
    <!-- Widescreen: a community's header, hosted outside the scroller so it cannot
         ride the list's overscroll. Empty (and zero-height) otherwise. -->
    <div id="ws-community-head"></div>
    <div id="chat-list"></div>
    <div class="fadeout-bottom" style="position: fixed; bottom: 65px;"></div>
</div>
