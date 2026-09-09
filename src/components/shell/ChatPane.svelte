<script>
    // The conversation pane's chrome. The header, pins drawer, message list, command strip
    // and composer are islands that mount into the hosts here; chat.js owns the wallpaper
    // controls and the scroll-return button by id.
    import { shellPanes } from '../lib/shell.svelte.js';
    const panes = shellPanes();
</script>

<div id="chat" class="chat" style:display={panes.chat ? null : 'none'}>
    <!-- Sits behind every other chat surface and absorbs the blur + brightness filter so
         messages stay crisp. Driven by CSS variables chat.js updates live during preview. -->
    <div id="chat-wallpaper-layer" class="chat-wallpaper-layer"></div>
    <div class="chat-header">
        <div id="chat-back-btn" class="btn nav-back-btn">
            <span class="icon icon-chevron-double-left nav-icon"></span>
            <span id="chat-back-notification-dot" class="update-notification-dot" style="display: none;"></span>
        </div>
        <div class="profile-header-info">
            <div class="profile-header-name-row">
                <div id="chat-header-avatar-container"></div>
                <h3 id="chat-contact" class="cutoff chat-contact-with-status btn"></h3>
            </div>
            <span class="cutoff chat-contact-status status-hidden btn" id="chat-contact-status"></span>
        </div>
        <div id="chat-pins-btn" class="btn pins-header-btn" title="Pinned Messages" style="display: none;">
            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                <path d="M8.3767 15.6163L2.71985 21.2732M11.6944 6.64181L10.1335 8.2027C10.0062 8.33003 9.94252 8.39369 9.86999 8.44427C9.80561 8.48917 9.73616 8.52634 9.66309 8.555C9.58077 8.58729 9.49249 8.60495 9.31592 8.64026L5.65145 9.37315C4.69915 9.56361 4.223 9.65884 4.00024 9.9099C3.80617 10.1286 3.71755 10.4213 3.75771 10.7109C3.8038 11.0434 4.14715 11.3867 4.83387 12.0735L11.9196 19.1592C12.6063 19.8459 12.9497 20.1893 13.2821 20.2354C13.5718 20.2755 13.8645 20.1869 14.0832 19.9928C14.3342 19.7701 14.4294 19.2939 14.6199 18.3416L15.3528 14.6771C15.3881 14.5006 15.4058 14.4123 15.4381 14.33C15.4667 14.2569 15.5039 14.1875 15.5488 14.1231C15.5994 14.0505 15.663 13.9869 15.7904 13.8596L17.3512 12.2987C17.4326 12.2173 17.4734 12.1766 17.5181 12.141C17.5578 12.1095 17.5999 12.081 17.644 12.0558C17.6936 12.0274 17.7465 12.0048 17.8523 11.9594L20.3467 10.8904C21.0744 10.5785 21.4383 10.4226 21.6035 10.1706C21.7481 9.95025 21.7998 9.68175 21.7474 9.42348C21.6875 9.12813 21.4076 8.84822 20.8478 8.28839L15.7047 3.14526C15.1448 2.58543 14.8649 2.30552 14.5696 2.24565C14.3113 2.19329 14.0428 2.245 13.8225 2.38953C13.5705 2.55481 13.4145 2.91866 13.1027 3.64636L12.0337 6.14071C11.9883 6.24653 11.9656 6.29944 11.9373 6.34905C11.9121 6.39313 11.8836 6.43522 11.852 6.47496C11.8165 6.51971 11.7758 6.56041 11.6944 6.64181Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
        <div id="chat-menu-btn" class="btn nav-menu-btn" title="Chat Options">
            <span class="icon icon-dots-horizontal nav-icon"></span>
        </div>
        <!-- Mirrors the Profile edit bar over the chat header while a wallpaper preview is staged. -->
        <div id="wallpaper-edit-bar" style="display: none;">
            <div id="wallpaper-edit-cancel-btn">
                <span class="icon icon-edit-x"></span>
                <span>Cancel</span>
            </div>
            <span id="wallpaper-edit-mode-label">Edit Mode is enabled.</span>
            <div id="wallpaper-edit-save-btn">
                <span class="icon icon-save"></span>
                <span>Save</span>
            </div>
        </div>
    </div>
    <!-- Pinned messages slide open under the header; pins.js mounts the rows island. -->
    <div id="pins-drawer" class="pins-drawer" style="display: none;">
        <div id="pins-drawer-list" class="pins-drawer-list"></div>
        <div id="pins-drawer-close" class="pins-drawer-close btn">
            <span>Click to Close</span>
            <svg class="pins-drawer-close-icon" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                <path d="M18 15L12 9L6 15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
    </div>
    <div id="msg-top-fade" class="fadeout-top-msgs" style="top: 60px;"></div>
    <div id="chat-messages" class="chat-messages"></div>
    <!-- In the chat's flex flow directly above #chat-box, so the composer stays visible
         while the blur + brightness sliders are tuned. -->
    <div id="wallpaper-preview-bar" class="wallpaper-preview-bar" style="display: none;">
        <div class="wallpaper-preview-sliders">
            <label class="wallpaper-slider" title="Blur">
                <span class="icon icon-eye-off wallpaper-slider-icon"></span>
                <input type="range" id="wallpaper-blur-slider" min="0" max="30" step="1" value="0">
            </label>
            <label class="wallpaper-slider" title="Brightness">
                <span class="icon icon-bulb wallpaper-slider-icon"></span>
                <input type="range" id="wallpaper-dim-slider" min="10" max="100" step="1" value="50">
            </label>
        </div>
    </div>
    <div class="row input-box" id="chat-box">
        <!-- Anchored to #chat-box's top edge so it rides up as the composer grows. -->
        <button id="chat-scroll-return" class="corner-float scroll-return-btn"><span class="icon icon-chevron-down"></span><span id="chat-scroll-return-badge" class="scroll-return-badge"></span></button>
        <div id="msg-bottom-fade" class="fadeout-bottom-msgs"></div>
        <div id="chat-reply-bar">
            <span id="chat-reply-bar-label">Replying to <span id="chat-reply-bar-name"></span></span>
            <span id="chat-reply-bar-snippet"></span>
            <button id="chat-reply-bar-cancel"><span class="icon icon-cancel"></span></button>
        </div>
        <div id="chat-command-bar">
            <span id="chat-command-bar-label">Using <span id="chat-command-bar-cmd"></span> with</span>
            <span id="chat-command-bar-bot"></span>
            <span id="chat-command-bar-hint"></span>
            <button id="chat-command-bar-cancel"><span class="icon icon-cancel"></span></button>
        </div>
        <div class="row chat-input-container">
            <button id="chat-input-file"><span class="icon icon-plus"></span></button>
            <button id="chat-input-cancel" style="display: none;"><span class="icon icon-cancel"></span></button>
            <div id="chat-input-host"></div>
            <button id="chat-input-emoji"><span class="icon icon-smile-face"></span></button>
            <button id="chat-input-voice" style="margin-right: 3px;"><span class="icon icon-mic-on"></span></button>
            <button id="chat-input-send" style="display: none; margin-right: 3px;"><span class="icon icon-send"></span></button>
        </div>
    </div>
</div>
