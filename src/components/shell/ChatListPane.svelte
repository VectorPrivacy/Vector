<script>
    // The list pane: bookmarks, the account row (profile.js fills it), the sync line,
    // the two New buttons, the widescreen community head and the list island's host.
    import { shellPanes } from '../lib/shell.svelte.js';
    const panes = shellPanes();
</script>

<div id="chats" class="chats" style:display={panes.chats ? null : 'none'}>
    <div id="chat-bookmarks-btn" class="btn chat-bookmarks-btn" style="display: none; z-index: 5;">
        <span class="icon icon-bookmark nav-icon"></span>
    </div>
    <div id="account" class="account" style="display: none;">
        <div class="profile-header-info">
            <div class="profile-header-name-row">
                <div id="account-avatar-container"></div>
                <h3 id="account-name" class="cutoff chat-contact-with-status btn"></h3>
            </div>
            <span id="account-status" class="cutoff chat-contact-status btn"></span>
        </div>
    </div>
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
