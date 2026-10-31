<script>
    // The Audit Log section of Community Settings: the community's signed history, newest
    // first, each act a sentence that opens onto what it changed.
    import { csState } from '../lib/community-settings.svelte.js';
    import { auditState, auditSetAction, auditSetQuery, auditShowMore, auditToggle, auditReportState, auditReportClose } from '../lib/audit-log.svelte.js';
    import { advancedState } from '../lib/advanced.svelte.js';
    import { FAMILIES, familyOf, toneOf, sentence, details, plainText, when, whenFull } from '../lib/audit-text.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';
    import Select from '../ui/Select.svelte';

    // h: realName(npub), avatarSrc(npub), loadAudit(communityId), syncReport(communityId), copyText(text, what)
    let { h } = $props();

    const st = csState();
    const audit = auditState();
    const adv = advancedState();
    const rep = auditReportState();

    // The sync report's rows worth a look first: refused, held back, or not this device's newest.
    const KINDS = { 0: 'community', 1: 'role', 2: 'channel', 3: 'grant', 4: 'banlist', 8: 'invite links', 10: 'dissolved', 11: 'pins', 13: 'role', 14: 'permissions', 15: 'category', 16: 'tenure', 17: 'channel sync', 18: 'writers' };
    const kindName = (k) => KINDS[k] || `kind ${k}`;
    const flagged = $derived.by(() => {
        const ents = rep.report?.data?.control?.entities || [];
        return ents.filter((e) => e.refused || e.carried || e.kept || e.mine
            || (e.newest && !e.newest.admitted)
            || (e.newest && e.floor && e.newest.version > e.floor.version && !(e.folds_to && e.folds_to.version >= e.newest.version)));
    });
    const reportText = $derived(rep.report?.data ? JSON.stringify(rep.report.data, null, 1) : '');

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
        <div class="audit-head-actions">
            {#if adv.on}
                <button class="cs-btn-text" disabled={rep.report?.loading} onclick={() => h.syncReport(st.communityId)}>Sync Report</button>
            {/if}
            <button class="cs-btn-text" disabled={audit.loading} onclick={() => h.loadAudit(st.communityId)}>Refresh</button>
        </div>
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

{#if rep.report}
    {@const c = rep.report.data?.control}
    {@const ep = rep.report.data?.epoch}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="sr-overlay" onclick={(e) => { if (e.target === e.currentTarget) auditReportClose(); }}>
        <div class="sr-card" role="dialog" aria-label="Sync Report">
            <header class="sr-head">
                <div>
                    <h3 class="sr-title">Sync Report</h3>
                    <p class="sr-sub">How this device reads the community. Versions and short IDs only, safe to share.</p>
                </div>
                <button class="cs-close" aria-label="Close" onclick={() => auditReportClose()}><span class="cs-close-x">&#x2715;</span></button>
            </header>
            <div class="sr-body">
                {#if rep.report.loading}
                    <p class="cmt-empty">Reading the control plane…</p>
                {:else if rep.report.error}
                    <p class="cs-hint cs-hint-error">{rep.report.error}</p>
                {:else}
                    <div class="sr-chips">
                        <span class="sr-chip">{rep.report.data.client} {rep.report.data.version}</span>
                        {#if c && !c.error}
                            <span class="sr-chip">Epoch {c.root_epoch}</span>
                            <span class="sr-chip" class:bad={!c.converged}>{c.converged ? 'Authority settled' : 'Authority unsettled'}</span>
                            <span class="sr-chip" class:bad={c.truncated}>{c.truncated ? 'Read cut short' : 'Read whole'}</span>
                            <span class="sr-chip">{c.editions} edits, {c.pages_read} pages</span>
                            {#if c.would_update}<span class="sr-chip bad">Unsaved changes</span>{/if}
                        {:else if c?.error}
                            <span class="sr-chip bad">Control: {c.error}</span>
                        {/if}
                        {#if ep?.error}<span class="sr-chip bad">Epoch: {ep.error}</span>{/if}
                    </div>
                    {#if flagged.length}
                        <h4 class="sr-label">Worth a look</h4>
                        <ul class="sr-rows">
                            {#each flagged as e (e.entity)}
                                <li>
                                    <strong>{kindName(e.kind)}</strong> {e.entity}{#if e.mine} (yours){/if}
                                    · held v{e.floor?.version ?? '–'} · newest v{e.newest?.version ?? '–'}{#if e.newest} by {e.newest.author}{/if}
                                    {#if e.refused}· refused: {e.refused}{/if}{#if e.carried}· kept, refused{/if}{#if e.kept}· kept{/if}{#if e.newest && !e.newest.admitted}· not admitted{/if}
                                </li>
                            {/each}
                        </ul>
                    {/if}
                    <h4 class="sr-label">Full report</h4>
                    <pre class="sr-pre">{reportText}</pre>
                {/if}
            </div>
            <footer class="sr-foot">
                <button class="cs-btn-text" onclick={() => auditReportClose()}>Close</button>
                <button class="cs-btn-save" disabled={!reportText} onclick={() => h.copyText(reportText, 'Sync Report')}>Copy Report</button>
            </footer>
        </div>
    </div>
{/if}

<style>
    .audit-head-actions { display: flex; gap: 4px; }
    .sr-overlay {
        position: fixed;
        inset: 0;
        z-index: 10001;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: max(16px, env(safe-area-inset-top)) 16px max(16px, env(safe-area-inset-bottom));
        background-color: rgba(0, 0, 0, 0.7);
    }
    .sr-card {
        display: flex;
        flex-direction: column;
        width: 100%;
        max-width: 720px;
        max-height: 100%;
        background-color: #101010;
        border: 1px solid #2c2e2f;
        border-radius: 18px;
        overflow: hidden;
    }
    .sr-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: 12px;
        padding: 16px 18px 12px;
        border-bottom: 1px solid rgba(57, 57, 57, 0.5);
    }
    .sr-title { margin: 0; font-size: 17px; font-weight: 600; }
    .sr-sub { margin: 4px 0 0; font-size: 13px; color: rgba(255, 255, 255, 0.55); }
    .sr-body { flex: 1; min-height: 0; overflow-y: auto; padding: 14px 18px; }
    .sr-chips { display: flex; flex-wrap: wrap; gap: 6px; }
    .sr-chip {
        font-size: 12px;
        padding: 4px 10px;
        border-radius: 999px;
        border: 1px solid rgba(255, 255, 255, 0.14);
        color: rgba(255, 255, 255, 0.8);
    }
    .sr-chip.bad { border-color: var(--danger-color); color: var(--danger-color); }
    .sr-label {
        margin: 16px 0 6px;
        font-size: 11px;
        font-weight: 700;
        letter-spacing: 0.12em;
        text-transform: uppercase;
        color: rgba(255, 255, 255, 0.45);
    }
    .sr-rows { margin: 0; padding-left: 18px; font-size: 13px; line-height: 1.55; color: rgba(255, 255, 255, 0.85); }
    .sr-pre {
        margin: 0;
        padding: 10px 12px;
        max-height: 320px;
        overflow: auto;
        font-size: 11px;
        line-height: 1.45;
        background-color: #070707;
        border: 1px solid rgba(255, 255, 255, 0.08);
        border-radius: 10px;
        white-space: pre;
        -webkit-user-select: text;
        user-select: text;
    }
    .sr-foot {
        display: flex;
        justify-content: flex-end;
        gap: 8px;
        padding: 12px 18px;
        border-top: 1px solid rgba(57, 57, 57, 0.5);
    }
</style>
