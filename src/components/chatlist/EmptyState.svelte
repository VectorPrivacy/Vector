<script>
    // The empty list: a Get Started card that rides the New Chat button, and Viktor's
    // welcome pointing at the Hub. Viktor's little state machine stays a leaf action.
    // Widescreen gives the welcome the empty conversation pane, which has the room for it.
    import { shellState } from '../lib/shell.svelte.js';
    let { h } = $props();   // h: newChat, openHub, bindViktor
    const shell = shellState();

    /** Show `node` inside the element `target` names, or back where it was rendered. */
    function portal(node, target) {
        const home = node.parentNode;
        const anchor = document.createComment('');
        home.insertBefore(anchor, node);
        const place = (t) => {
            const dest = t && document.querySelector(t);
            if (dest) dest.appendChild(node);
            else if (anchor.parentNode) anchor.parentNode.insertBefore(node, anchor);
        };
        place(target);
        return {
            update: place,
            destroy() { node.remove(); anchor.remove(); },
        };
    }
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
</div>
<div class="chatlist-intro" use:portal={shell.ws ? '#ws-conv-empty' : null}>
    <img class="chatlist-intro-viktor" alt="Viktor" use:h.bindViktor>
    <div class="chatlist-intro-text">
        <h4>Welcome to Vector!</h4>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <p>Feel free to <span class="chatlist-intro-link" onclick={h.openHub}>join the public community</span> to learn more about Vector, discuss privacy, and make some new friends.</p>
    </div>
</div>
