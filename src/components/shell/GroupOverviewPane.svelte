<script>
    // The community overview's frame: header (community.js fills it) and the scroll host
    // the CommunityOverview island mounts into.
    import { shellPanes } from '../lib/shell.svelte.js';
    const panes = shellPanes();
</script>

<div id="group-overview" class="chats" style:display={panes.groupOverview ? null : 'none'}>
    <div class="chat-header" style="top: 0px;">
        <div id="group-overview-back-btn" class="btn nav-back-btn">
            <span class="icon icon-chevron-double-left nav-icon"></span>
        </div>
        <div class="profile-header-info">
            <div class="profile-header-name-row">
                <div id="group-overview-header-avatar-container">
                    <img src="./icons/group-placeholder.svg" alt="">
                </div>
                <h3 id="group-overview-name" class="cutoff chat-contact-with-status btn"></h3>
            </div>
            <span id="group-overview-status" class="cutoff chat-contact-status btn"></span>
        </div>
    </div>
    <div id="group-overview-scroll" style="overflow-y: auto; height: 100%; position: relative;"></div>
</div>
