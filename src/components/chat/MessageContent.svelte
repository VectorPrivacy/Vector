<script>
    // A message's body: text, attachments, the cards a message can carry, and the
    // trailing marks. The composition and the marks are Svelte; text, attachments and
    // the cards are the app's builders as leaves, rebuilt whole when the content
    // signature changes (a reaction echo hands the row a new object with the same
    // content, and a rebuild would reset playback and spoiler reveals).
    import Attachments from './Attachments.svelte';

    let { msg, sender, ctx, h, sig } = $props();
    // h: buildText(msg, ctx) → span | null, the Attachments leaves, buildCryptoAddress(msg),
    //    renderEmojiPackPreviews(node, text), renderCommunityInvitePreviews(node, text), xdcUrl(msg, ctx),
    //    renderXdcUrlCard(node, msg, url), webPreviewsEnabled(), buildLinkPreview(msg), isAndroid(),
    //    fmtCountdown(secs), selfDestructTooltip(el), selfDestructTooltipEnd()

    // One built element into a display:contents host.
    function leaf(node, el) {
        node.replaceChildren(el || []);
    }
    function into(node, fill) {
        fill(node);
    }

    const live = $derived(!msg.pending && !msg.failed && !ctx.revealedBlocked);
    const xdcUrl = $derived(live ? h.xdcUrl(msg) : null);
    // A vectorapp.io profile link is already a mention pill; an OpenGraph card would repeat it.
    const showLinkPreview = $derived(live && h.webPreviewsEnabled() && !xdcUrl
        && !/https?:\/\/vectorapp\.io\/profile\/npub1[a-z0-9]{58}/i.test(msg.content || ''));
    const selfDestructRemaining = $derived(msg.expiration ? msg.expiration - Math.floor(Date.now() / 1000) : 0);
</script>

{#key sig}
    {@const text = h.buildText(msg, ctx)}
    {@const crypto = h.buildCryptoAddress(msg)}
    {#if text}
        <span style="display:contents" use:leaf={text}></span>
    {/if}
{/key}
<!-- Outside the key: attachments derive on their own, so a send finishing keeps its media. -->
{#if msg.attachments?.length}
    <div class="dmsg-attachments"><Attachments {msg} {sender} {ctx} {h} /></div>
{/if}
{#key sig}
    {#if crypto}
        <span style="display:contents" use:leaf={crypto}></span>
    {/if}
    {#if msg.content}
        <span style="display:contents" use:into={(node) => h.renderEmojiPackPreviews(node, msg.content)}></span>
        <span style="display:contents" use:into={(node) => h.renderCommunityInvitePreviews(node, msg.content)}></span>
    {/if}
    {#if xdcUrl}
        <!-- An .xdc link is a playable card, and supersedes the OpenGraph card. -->
        <div use:into={(node) => h.renderXdcUrlCard(node, msg, xdcUrl)}></div>
    {/if}
    {#if showLinkPreview}
        {@const preview = h.buildLinkPreview(msg)}
        {#if preview}
            <span style="display:contents" use:leaf={preview}></span>
        {/if}
    {/if}
    {#if msg.edited}
        <span class="dmsg-edited" class:btn={msg.edit_history?.length > 0}
              data-msg-id={msg.edit_history?.length > 0 ? msg.id : undefined}
              title={msg.edit_history?.length > 0 ? 'Click to view edit history' : undefined}>(edited)</span>
    {/if}
    {#if msg.mine}
        {#if msg.failed}
            <span class="dmsg-status dmsg-status-failed">Failed · <span class="dmsg-failed-action" data-action="retry">Retry</span> · <span class="dmsg-failed-action" data-action="delete">Delete</span></span>
        {:else if msg.pending}
            <span class="dmsg-status">Sending...</span>
        {:else}
            <span class="dmsg-status">Sent <span class="icon icon-check-circle"></span></span>
        {/if}
    {/if}
    {#if msg.expiration}
        <!-- Inline SVG, not an .icon: .icon is absolutely inset and would escape this span. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <span class="dmsg-selfdestruct" data-expiration={String(msg.expiration)}
              onmouseenter={h.isAndroid() ? null : (e) => h.selfDestructTooltip(e.currentTarget)}
              onmouseleave={h.isAndroid() ? null : () => h.selfDestructTooltipEnd()}>
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/></svg>
            {#if h.isAndroid()}
                <!-- Touch has no hover: the countdown sits beside the clock. -->
                <span class="dmsg-selfdestruct-time">{selfDestructRemaining > 0 ? h.fmtCountdown(selfDestructRemaining) : ''}</span>
            {/if}
        </span>
    {/if}
{/key}
