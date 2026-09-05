<script>
    // One message row (Phase 2, CHAT_VIEW_ISLAND_DESIGN.md). The row owns its SHELL:
    // root attributes, gutter avatar, header (author, bot mark, badges, time) and the
    // body layout. Those derive from the message plus the author's profile signal and
    // the community signal, so a resolved profile or a granted role repaints them with
    // no retro-resolve code. The content (text, attachments, previews, status) is
    // filled by the vanilla builders through an action, byte-identical to
    // renderMessage, and re-filled whole by `update(msg)`; the reactions row is filled
    // once here and reconciled in place by the vanilla reconciler afterwards.
    //
    // The list container, the windowing engine and every scroll measurement stay
    // vanilla: they only need a child element whose id is the message id. Attributes
    // vanilla writes after render (data-streak, data-derezzing, data-jumped, swipe
    // transforms, the has-reply class after an update) are deliberately not re-bound
    // here, so those writes persist.
    import { profileVersion, communityVersion } from '../lib/signals.svelte.js';

    let {
        msg,               // raw message, shared by reference with the chat's array
        sender = null,     // the row author's profile as the caller resolved it
        streak = 'first',  // computed by the caller from the row above (vanilla owns streaks)
        replyEl = undefined, // prebuilt .dmsg-reply element, null, or undefined = build it here
        replyPending = '',   // replied_to id when the quoted parent has not arrived yet
        ctx,               // { myNpub, isGroupChat, currentChat, pinged, replyingTo, revealedBlocked }
        h,                 // vanilla helpers: getProfile, getName, getProfileAvatarSrc, twemojify,
                           //   showTooltip, hideTooltip, formatHourMinute, fillContent, fillReactions
    } = $props();

    // The live message. In the list island the `msg` prop itself changes (the array
    // holds the new object); mounted standalone, update() swaps it in. Either way the
    // shell re-derives from `current` and the content refills whole.
    let override = $state.raw(null);
    const current = $derived(override || msg);

    /** The message changed (edit, status, attachment, id swap): re-derive and refill. */
    export function update(next) {
        override = next;
    }

    // Variants that keep the row shell but replace its body: a PIVX payment bubble,
    // and a blocked author's placeholder (until revealed).
    // svelte-ignore state_referenced_locally
    const isPivx = !!msg.pivx_payment;
    const isBlocked = $derived(!!ctx.blocked);

    // Mount-time constants. Authorship never changes for a row; a changed message
    // keeps its author.
    // svelte-ignore state_referenced_locally
    const authorFullId = msg.mine ? ctx.myNpub : (msg.npub || sender?.id || '');
    // svelte-ignore state_referenced_locally
    const shortSender = (msg.mine ? ctx.myNpub : (sender?.id || msg.npub || '')).substring(0, 8);
    // svelte-ignore state_referenced_locally
    const communityId = ctx.currentChat?.metadata?.custom_fields?.community_id || null;
    // The reactions row exists from mount only if the message had reactions then;
    // the vanilla reconciler adds and removes it afterwards.
    // svelte-ignore state_referenced_locally
    const hadReactions = !!msg.reactions?.length;
    // The quoted parent: prebuilt by the caller, or built here (list island).
    // svelte-ignore state_referenced_locally
    const reply = replyEl !== undefined ? replyEl : (msg.replied_to && h.buildReply ? h.buildReply(msg, sender) : null);
    // svelte-ignore state_referenced_locally
    const pendingReply = replyPending || (replyEl === undefined && msg.replied_to && !reply ? msg.replied_to : '');

    const status = $derived(current.failed ? 'failed' : current.pending ? 'pending' : 'sent');
    const hourMinute = $derived(h.formatHourMinute(current.at));

    // The author as the profile store knows them now. `sender` is the mount-time
    // snapshot; the signal read is what repaints the row when the profile lands.
    // One fresh view-model per derivation: the profile object itself is mutated in
    // place and keeps its identity, which a per-field derived would read as "unchanged".
    const author = $derived.by(() => {
        profileVersion(authorFullId);
        const profile = msg.mine
            ? (h.getProfile(ctx.myNpub) || null)
            : (h.getProfile(authorFullId) || sender || null);
        return {
            name: h.getName(profile || authorFullId),
            src: h.getProfileAvatarSrc(profile) || null,
            bot: !!profile?.bot,
        };
    });
    const displayName = $derived(author.name);
    const avatarSrc = $derived(author.src);
    const isBot = $derived(author.bot);

    // Rank badges follow the community's roster, not the mount-time snapshot.
    const rank = $derived.by(() => {
        if (communityId) communityVersion(communityId);
        const chat = ctx.currentChat;
        const admin = ctx.isGroupChat && !!chat?.metadata?.admins?.includes(authorFullId);
        const owner = !!(chat?.metadata?.custom_fields?.owner_npub && authorFullId
            && chat.metadata.custom_fields.owner_npub === authorFullId);
        return { admin, owner };
    });

    // ── actions: the vanilla leaf builders ──

    // O(1) back-reference the toolbar, streak and reaction paths read; follows updates.
    function expando(node, m) {
        node._dmsgMsg = m;
        return { update: (next) => { node._dmsgMsg = next; } };
    }

    // Name text + twemoji. Keyed on the string so a re-derive that yields the same
    // name never touches the node (twemojify would restart image loads).
    function name(node, text) {
        let cur;
        const render = (t) => {
            if (t === cur) return;
            cur = t;
            node.textContent = t;
            h.twemojify(node);
        };
        render(text);
        return { update: render };
    }

    // The avatar is a direct child of the gutter; a load failure swaps in the
    // placeholder the way createAvatarImg does.
    let avatarFailed = $state(false);
    $effect(() => { avatarSrc; avatarFailed = false; });

    // The quoted parent sits ABOVE the header as the body's first child.
    function replyInto(node) {
        if (reply) node.insertBefore(reply, node.firstChild);
    }

    // Content: exactly renderMessage's builders; refilled whole when the message changes.
    function contentInto(node, m) {
        let cur = m;
        h.fillContent(node, cur, sender, ctx);
        return {
            update: (next) => {
                if (next === cur) return;
                cur = next;
                node.replaceChildren();
                h.fillContent(node, cur, sender, ctx);
            },
        };
    }
    function reactionsInto(node) {
        h.fillReactions(node, msg);
    }
    function pivxInto(node) {
        node.replaceChildren(h.buildPivxBubble(msg));
    }
    function blockedInto(node) {
        node.replaceChildren(h.buildBlockedPlaceholder(msg));
    }
</script>

<div
    class="dmsg"
    class:dmsg--has-reply={!!reply}
    id={current.id}
    data-sender={shortSender}
    data-mine={msg.mine ? 'true' : 'false'}
    data-status={status}
    data-at={current.at ? String(current.at) : undefined}
    data-streak={streak}
    data-pinged={ctx.pinged ? 'true' : undefined}
    data-replying-to={ctx.replyingTo ? 'true' : undefined}
    data-reply-pending={pendingReply || undefined}
    style:opacity={ctx.revealedBlocked ? '0.4' : null}
    use:expando={current}
>
    <div class="dmsg-gutter">
        {#if avatarSrc && !avatarFailed}
            <!-- svelte-ignore a11y_missing_attribute (byte-identical to createAvatarImg's output) -->
            <img
                class="dmsg-avatar btn"
                src={avatarSrc}
                data-npub={authorFullId || undefined}
                style="width: 40px; height: 40px; object-fit: cover; border-radius: 50%; margin: 0px;"
                onerror={() => (avatarFailed = true)}
            />
        {:else}
            <div
                class="placeholder-avatar dmsg-avatar btn"
                data-npub={authorFullId || undefined}
                style="min-height: 40px; min-width: 40px; max-height: 40px; max-width: 40px; background-image: url(&quot;icons/user-placeholder.svg&quot;); background-size: cover; background-position: center center; margin: 0px;"
            ></div>
        {/if}
        <time class="dmsg-time-hover">{hourMinute}</time>
    </div>
    {#if isPivx}
        <div class="dmsg-body" use:pivxInto></div>
    {:else if isBlocked}
        <div class="dmsg-body">
            <div class="dmsg-header">
                <span class="dmsg-author btn" data-npub={authorFullId || undefined} use:name={displayName}></span>
                {#if isBot}
                    <!-- svelte-ignore a11y_no_static_element_interactions -->
                    <span class="icon icon-bot dmsg-author-bot-icon" onmouseenter={(e) => h.showTooltip('Bot', e.currentTarget)} onmouseleave={() => h.hideTooltip()}></span>
                {/if}
                {#if rank.admin}<span class="dmsg-author-badge admin">admin</span>{/if}
                {#if rank.owner}<span class="dmsg-author-badge owner">Owner</span>{/if}
                <time class="dmsg-time">{hourMinute}</time>
            </div>
            <div class="dmsg-content" use:blockedInto></div>
        </div>
    {:else}
    <div class="dmsg-body" use:replyInto>
        <div class="dmsg-header">
            <span class="dmsg-author btn" data-npub={authorFullId || undefined} use:name={displayName}></span>
            {#if isBot}
                <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla tooltip span) -->
                <span
                    class="icon icon-bot dmsg-author-bot-icon"
                    onmouseenter={(e) => h.showTooltip('Bot', e.currentTarget)}
                    onmouseleave={() => h.hideTooltip()}
                ></span>
            {/if}
            {#if rank.admin}
                <span class="dmsg-author-badge admin">admin</span>
            {/if}
            {#if rank.owner}
                <span class="dmsg-author-badge owner">Owner</span>
            {/if}
            <time class="dmsg-time">{hourMinute}</time>
        </div>
        <div class="dmsg-content" use:contentInto={current}></div>
        {#if hadReactions}
            <div class="dmsg-reactions" use:reactionsInto></div>
        {/if}
    </div>
    {/if}
</div>

<!-- No <style>: the global .dmsg-* rules cascade; the DOM matches renderMessage's output. -->
