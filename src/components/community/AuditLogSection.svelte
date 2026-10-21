<script>
    // The Audit Log section of Community Settings: the community's signed history, newest
    // first, each act a sentence that opens onto what it changed.
    import { csState } from '../lib/community-settings.svelte.js';
    import { auditState, auditSetAction, auditSetQuery, auditShowMore, auditToggle } from '../lib/audit-log.svelte.js';
    import { FAMILIES, familyOf, toneOf, sentence, details, plainText, when, whenFull } from '../lib/audit-text.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';
    import Select from '../ui/Select.svelte';

    // h: realName(npub), avatarSrc(npub), loadAudit(communityId)
    let { h } = $props();

    const st = csState();
    const audit = auditState();

    // Read on first sight of this community's section, and again only on Refresh.
    $effect(() => {
        if (st.communityId && audit.communityId !== st.communityId && !audit.loading) h.loadAudit(st.communityId);
    });

    const shortNpub = (npub) => (npub ? `${npub.slice(0, 12)}…${npub.slice(-6)}` : 'Someone');
    const ctx = {
        member: (npub) => {
            if (!npub) return 'someone';
            profileVersion(npub);
            return h.realName(npub) || shortNpub(npub);
        },
    };

    const rows = $derived.by(() => {
        const q = audit.query.trim().toLowerCase();
        const out = [];
        for (const e of audit.entries) {
            if (audit.action !== 'all' && familyOf(e.action) !== audit.action) continue;
            if (q && !plainText(e, ctx).toLowerCase().includes(q)) continue;
            out.push(e);
        }
        return out;
    });
    const visible = $derived(rows.slice(0, audit.shown));
</script>

<section class="cs-block" data-anchor="audit-list">
    <div class="cs-heading-row">
        <h3 class="cs-heading">Audit Log</h3>
        <button class="cs-btn-text" disabled={audit.loading} onclick={() => h.loadAudit(st.communityId)}>Refresh</button>
    </div>
    <p class="cs-lede">Every change to this community, signed by whoever made it.</p>
    <div class="cs-bans-bar">
        <div class="emoji-search-container cs-bans-search">
            <span class="emoji-search-icon icon icon-search"></span>
            <input type="text" placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                   value={audit.query} oninput={(e) => auditSetQuery(e.currentTarget.value)}>
        </div>
        <Select class="audit-filter" label="Filter by action" options={FAMILIES} value={audit.action} onchange={auditSetAction} />
    </div>

    {#if audit.loading && !audit.entries.length}
        <p class="cmt-empty audit-empty">Reading this community's history…</p>
    {:else if audit.error && !audit.entries.length}
        <p class="cs-hint cs-hint-error audit-empty">{audit.error}</p>
    {:else}
        <div class="audit-list" class:busy={audit.loading}>
            {#each visible as e (e.id)}
                {@const lines = details(e, ctx)}
                {@const open = audit.open.has(e.id)}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="audit-row tone-{toneOf(e.action)}" class:openable={lines.length > 0} class:open
                     onclick={lines.length ? () => auditToggle(e.id) : null}>
                    <div class="audit-avatar">
                        <Avatar src={e.actor ? h.avatarSrc(e.actor) : null} size={32} />
                        <span class="audit-badge"></span>
                    </div>
                    <div class="audit-body">
                        <div class="audit-line">{#each sentence(e, ctx) as p}{#if p.b}<strong>{p.t}</strong>{:else}{p.t}{/if}{/each}</div>
                        <div class="audit-when" title={whenFull(e.at)}>{when(e.at)}</div>
                    </div>
                    {#if lines.length}<span class="audit-caret"><span class="icon icon-chevron-down"></span></span>{/if}
                </div>
                {#if open && lines.length}
                    <ul class="audit-details">
                        {#each lines as line}<li>{line}</li>{/each}
                    </ul>
                {/if}
            {:else}
                <p class="cmt-empty audit-empty">{audit.query.trim() || audit.action !== 'all' ? 'No matches.' : 'Nothing has happened here yet.'}</p>
            {/each}
        </div>
        {#if rows.length > visible.length}
            <button class="cs-btn-text audit-more" onclick={() => auditShowMore()}>Show more</button>
        {/if}
        {#if audit.compacted}
            <p class="cs-hint">Some history from before a key rotation this device didn't live through can't be read back.</p>
        {/if}
    {/if}
</section>
