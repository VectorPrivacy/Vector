<script>
    // The empty list: a Get Started card that rides the New Chat button, and Viktor's
    // welcome. Widescreen gives the welcome the empty conversation pane, which has the room
    // for it: App renders it there while this is up. Rendered twice rather than moved: a
    // node Svelte placed has to stay where it was put.
    import { shellState, setWelcome } from '../lib/shell.svelte.js';
    import Welcome from './Welcome.svelte';
    let { h } = $props();   // h: newChat, openHub, bindViktor
    const shell = shellState();

    $effect(() => {
        setWelcome(true);
        return () => setWelcome(false);
    });
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
{#if !shell.ws}<Welcome {h} />{/if}
