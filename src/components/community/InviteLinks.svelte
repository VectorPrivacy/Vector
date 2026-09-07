<script>
    // The Invite Links section: the folded mode pill, other creators' counts (collapsed past one),
    // my links with join counts, copy and revoke, and the create button.
    import { ilState, ilLinks, ilSummary, ilSetCopied } from '../lib/invitelinks.svelte.js';
    let { h } = $props();
    const st = ilState();
    const links = $derived(ilLinks());
    const summary = $derived(ilSummary());
    const others = $derived((summary.creators || []).filter(c => c.npub !== h.myNpub && (c.count || 0) > 0));
    const othersTotal = $derived(others.reduce((n, c) => n + (c.count || 0), 0));
    let expanded = $state(false);
    let copyTimer;

    function label(link) {
        const lbl = (link.label || '').trim();
        return lbl || `Invite · ${(link.token || link.url).slice(-8)}`;
    }
    function copy(link) {
        navigator.clipboard.writeText(link.url);
        ilSetCopied(link.token);
        clearTimeout(copyTimer);
        copyTimer = setTimeout(() => ilSetCopied(null), 1200);
    }
</script>

<section class="cmt-section">
    <div class="cmt-section-head">
        <span class="icon icon-share"></span>
        <div>
            <p class="cmt-section-title">Invite Links <span class="cmt-mode-pill" class:is-public={summary.is_public}
                title={summary.is_public ? 'Anyone with a link can join' : 'Invite-only — no public links'}>{summary.is_public ? 'Public' : 'Private'}</span></p>
            <p class="cmt-section-desc">Anyone with a link can join. Revoke every link to go private again.</p>
        </div>
    </div>
    <div>
        {#if others.length}
            <div class="cmt-others" class:collapsed={others.length > 1 && !expanded}>
                {#if others.length > 1 && !expanded}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
                    <p class="cmt-other-line cmt-others-toggle" onclick={() => { expanded = true; }}>View {othersTotal} other invite{othersTotal === 1 ? '' : 's'}</p>
                {:else}
                    {#each others as c (c.npub)}
                        <p class="cmt-other-line">{h.name(c.npub)} has {c.count} active invite link{c.count === 1 ? '' : 's'}</p>
                    {/each}
                {/if}
            </div>
        {/if}
        {#if !links.length}
            <p class="cmt-empty">{others.length ? 'You have no links of your own yet. Create one to start inviting.' : 'No active links yet. Create one to start inviting.'}</p>
        {/if}
        {#each links as link (link.token || link.url)}
            {@const joins = link.join_count || 0}
            <div class="cmt-link-row">
                <span class="cmt-link-url" title={link.url}><span class="icon icon-share cmt-link-glyph"></span>{label(link)}</span>
                <span class="cmt-link-count" title="{joins} member{joins === 1 ? '' : 's'} joined via this link"><span class="cmt-link-count-ico"><span class="icon icon-users-multi"></span></span>{joins}</span>
                <button class="cmt-icon-btn" class:cmt-copied={st.copied === link.token} title="Copy link" disabled={st.busy} onclick={() => copy(link)}>
                    <span class="icon {st.copied === link.token ? 'icon-check' : 'icon-copy'}"></span>
                </button>
                <button class="cmt-icon-btn cmt-icon-btn-danger" title="Revoke link" disabled={st.busy} onclick={() => h.revoke(link)}>
                    {#if st.revoking === link.token}<span class="icon icon-loading spin"></span>{:else}<span class="icon icon-trash"></span>{/if}
                </button>
            </div>
        {/each}
    </div>
    <button class="cmt-btn cmt-btn-secondary" disabled={st.busy} onclick={h.create}>
        {#if st.creating}<span class="icon icon-loading spin"></span>Creating…{:else}<span class="icon icon-plus"></span>Create invite link{/if}
    </button>
</section>
