<script>
    // The New Chat screen: paste an npub or an invite link, or scan one. The start
    // button shows once there is text; Enter starts. Routing belongs to the caller.
    let { h } = $props();
    // h: back(), start(text), scan(), helpEnter(el), helpLeave()

    let value = $state('');
    function start() {
        const text = value.trim();
        value = '';
        h.start(text);
    }
    function onKeydown(evt) {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            start();
        }
    }
</script>

<div class="chat-new-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn chat-new-back-text-btn" onclick={() => h.back()}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
        <p class="chat-new-back-text">Back</p>
    </div>
</div>
<center class="chat-new-center-content">
    <span class="icon icon-chat-bubble chat-new-icon"></span>
    <h2 class="chat-new-subtitle">Create New Chat</h2>
    <hr class="divider chat-new-subtitle-divider">
    <div class="chat-new-content">
        <span class="chat-new-description">Enter your contact’s nPub Key below to begin a new chat with them.<br><br>This action will also add them as a contact.</span>
    </div>
    <div class="chat-new-help">
        <a href="https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/features/add-contacts" target="_blank" class="chat-new-help-link"
           onmouseenter={(e) => h.helpEnter(e.currentTarget)} onmouseleave={() => h.helpLeave()}>
            <span class="icon icon-help chat-new-help-icon"></span>
        </a>
        <span class="chat-new-help-title">Need Help?</span>
    </div>
    <div class="chat-new-help-steps">
        <span class="chat-new-help-step">Profile</span>
        <span class="chat-new-help-arrow">›</span>
        <span class="chat-new-help-step">nPub Key</span>
        <span class="chat-new-help-arrow">›</span>
        <span class="chat-new-help-step">Copy</span>
    </div>
</center>
<div class="row input-box" id="chat-new-box">
    <div class="row chat-input-container">
        <input id="chat-new-input" type="text" placeholder="Paste nPub Key or Invite Link..." bind:value onkeydown={onKeydown}>
        {#if value.length > 0}
            <button id="chat-new-btn" style="margin-right: 3px;" onclick={start}><span class="icon icon-add-user"></span></button>
        {/if}
        <button id="chat-new-scan-btn" aria-label="Scan QR code" onclick={() => h.scan()}>
            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                <path d="M6.5 6.5H6.51M17.5 6.5H17.51M6.5 17.5H6.51M13 13H13.01M17.5 17.5H17.51M17 21H21V17M14 16.5V21M21 14H16.5M15.6 10H19.4C19.9601 10 20.2401 10 20.454 9.89101C20.6422 9.79513 20.7951 9.64215 20.891 9.45399C21 9.24008 21 8.96005 21 8.4V4.6C21 4.03995 21 3.75992 20.891 3.54601C20.7951 3.35785 20.6422 3.20487 20.454 3.10899C20.2401 3 19.9601 3 19.4 3H15.6C15.0399 3 14.7599 3 14.546 3.10899C14.3578 3.20487 14.2049 3.35785 14.109 3.54601C14 3.75992 14 4.03995 14 4.6V8.4C14 8.96005 14 9.24008 14.109 9.45399C14.2049 9.64215 14.3578 9.79513 14.546 9.89101C14.7599 10 15.0399 10 15.6 10ZM4.6 10H8.4C8.96005 10 9.24008 10 9.45399 9.89101C9.64215 9.79513 9.79513 9.64215 9.89101 9.45399C10 9.24008 10 8.96005 10 8.4V4.6C10 4.03995 10 3.75992 9.89101 3.54601C9.79513 3.35785 9.64215 3.20487 9.45399 3.10899C9.24008 3 8.96005 3 8.4 3H4.6C4.03995 3 3.75992 3 3.54601 3.10899C3.35785 3.20487 3.20487 3.35785 3.10899 3.54601C3 3.75992 3 4.03995 3 4.6V8.4C3 8.96005 3 9.24008 3.10899 9.45399C3.20487 9.64215 3.35785 9.79513 3.54601 9.89101C3.75992 10 4.03995 10 4.6 10ZM4.6 21H8.4C8.96005 21 9.24008 21 9.45399 20.891C9.64215 20.7951 9.79513 20.6422 9.89101 20.454C10 20.2401 10 19.9601 10 19.4V15.6C10 15.0399 10 14.7599 9.89101 14.546C9.79513 14.3578 9.64215 14.2049 9.45399 14.109C9.24008 14 8.96005 14 8.4 14H4.6C4.03995 14 3.75992 14 3.54601 14.109C3.35785 14.2049 3.20487 14.3578 3.10899 14.546C3 14.7599 3 15.0399 3 15.6V19.4C3 19.9601 3 20.2401 3.10899 20.454C3.20487 20.6422 3.35785 20.7951 3.54601 20.891C3.75992 21 4.03995 21 4.6 21Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </button>
    </div>
</div>
