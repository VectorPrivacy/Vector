<script>
    // Your own row. Ids stay: styles.css and widescreen.css select the row and its
    // parts by id in both homes. The caret is a widescreen-only affordance: the name
    // and status already have their own meanings, so switching gets its own target.
    import Avatar from '../ui/Avatar.svelte';
    import { accountState, accountHandlers } from '../lib/account.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    const st = accountState();
    const shell = shellState();
    const h = () => accountHandlers();
    let row = $state(null);

    function nameInto(node, [text, hasName, hh]) {
        const render = ([t, hn, fns]) => { node.textContent = t; if (hn) fns.twemojify?.(node); };
        render([text, hasName, hh]);
        return { update: render };
    }
    function statusInto(node, [text, tags, hh]) {
        const render = ([t, tg, fns]) => {
            node.textContent = t;
            fns.twemojify?.(node);
            fns.renderCustomEmojiShortcodes?.(node, tg || []);
        };
        render([text, tags, hh]);
        return { update: render };
    }
    // The intro fade is a class that must come off on animationend, or a later
    // display toggle replays it.
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

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="account" class="account" bind:this={row} style:display={st.visible ? null : 'none'} use:fadeIn={st.revealTick}>
    <div class="profile-header-info">
        <div class="profile-header-name-row">
            <div id="account-avatar-container">
                <Avatar src={st.avatarSrc} size={22} class="btn" />
            </div>
            <!-- The name lands through the action (twemoji needs the DOM); the row's real door is the avatar. -->
            <!-- svelte-ignore a11y_missing_content, a11y_no_noninteractive_element_interactions -->
            <h3 id="account-name" class="cutoff chat-contact-with-status btn" use:nameInto={[st.name, st.hasName, h()]} onclick={() => h().openProfile?.()}></h3>
        </div>
        <span id="account-status" class="cutoff chat-contact-status btn" use:statusInto={[st.statusText, st.emojiTags, h()]} onclick={() => h().setStatus?.()}></span>
    </div>
    {#if shell.ws}
        <div class="ws-account-caret btn" title="Switch account" onclick={(e) => { e.stopPropagation(); h().switchAccount?.(row); }}></div>
    {/if}
</div>
