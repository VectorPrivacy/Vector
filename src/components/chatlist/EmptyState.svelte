<script>
    // The empty list: a Get Started card that rides the New Chat button, and Viktor's
    // welcome pointing at the Hub. Viktor's little state machine stays a leaf action.
    let { h } = $props();   // h: newChat, openHub, bindViktor
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="chatlist-get-started btn" role="button" tabindex="0" onclick={h.newChat} onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); h.newChat(); } }}>
    <div class="chatlist-get-started-badge">
        <span class="icon icon-add-user"></span>
    </div>
    <div class="chatlist-get-started-text">
        <h4>Get Started</h4>
        <p>Create your first private chat.</p>
    </div>
    <div class="chatlist-get-started-watermark">
        <span class="icon icon-add-user"></span>
    </div>
</div>
<div class="chatlist-intro">
    <img class="chatlist-intro-viktor" alt="Viktor" use:h.bindViktor>
    <div class="chatlist-intro-text">
        <h4>Welcome to Vector!</h4>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <p>Feel free to <span class="chatlist-intro-link" onclick={h.openHub}>join the public community</span> to learn more about Vector, discuss privacy, and make some new friends.</p>
    </div>
</div>
