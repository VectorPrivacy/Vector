<script>
    // Viktor's welcome, pointing at the Hub. His little state machine stays a leaf action.
    let { h } = $props();   // h: openHub, bindViktor
</script>

<div class="chatlist-intro">
    <img class="chatlist-intro-viktor" alt="Viktor" use:h.bindViktor>
    <div class="chatlist-intro-text">
        <h4>Welcome to Vector!</h4>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <p>Feel free to <span class="chatlist-intro-link" onclick={h.openHub}>join the public community</span> to learn more about Vector, discuss privacy, and make some new friends.</p>
    </div>
</div>
