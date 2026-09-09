<script>
    // The member rows of the moderation console: standing rail, keep tick, avatar, name and
    // badge, tenure line and the cited reason. A click flips the member's keep state.
    import { modState, modIntel, modKeep, modToggleKeep } from '../lib/moderation.svelte.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    let { h } = $props();   // h: displayName(npub), avatarSrc(npub), ago(secs)

    const st = modState();
    const intel = $derived(modIntel());
    const keep = $derived(modKeep());
    const RESTATES_BADGE = new Set(['Long-standing member', 'Holds a role', 'Community owner']);

    const rows = $derived.by(() => {
        if (!intel) return [];
        const q = st.query.trim().toLowerCase();
        return intel.report.members.filter(x => {
            if (st.filter === 'cut' && keep.has(x.npub)) return false;
            if (st.filter === 'keep' && !keep.has(x.npub)) return false;
            if (q) { profileVersion(x.npub); if (!(h.displayName(x.npub) + ' ' + x.npub).toLowerCase().includes(q)) return false; }
            return true;
        });
    });
    function standing(x) {
        return x.verdict === 'protected' ? 'staff' : x.verdict === 'suspect' ? 'flagged' : x.verdict === 'trusted' ? 'trusted' : 'plain';
    }
    // A badge only where it adds something you can't infer from the group.
    function badge(x) {
        if (x.is_owner) return 'OWNER';
        if (x.is_me) return 'YOU';
        if (x.is_admin) return 'STAFF';
        if (x.verdict === 'trusted') return 'REGULAR';
        if (x.verdict === 'neutral' && x.reasons.length) return 'CHECK';
        return null;
    }
    function name(x) { profileVersion(x.npub); return h.displayName(x.npub); }
    function src(x) { profileVersion(x.npub); return h.avatarSrc(x.npub); }
</script>

{#if st.loading}
    <div class="mod-empty"><span class="icon icon-loading spin"></span></div>
{:else if st.error}
    <div class="mod-empty">{st.error}</div>
{:else if !rows.length}
    <div class="mod-empty">No members match.</div>
{:else}
    {#each rows as x (x.npub)}
        {@const kept = keep.has(x.npub)}
        {@const locked = x.verdict === 'protected'}
        {@const why = x.reasons.filter(r => !RESTATES_BADGE.has(r))}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="mod-row mod-standing-{standing(x)}" class:cutting={!kept} class:locked data-npub={x.npub}
             style:cursor={locked ? null : 'pointer'} title={locked ? (x.reasons[0] || 'Protected') : undefined}
             onclick={locked || st.busy ? null : () => modToggleKeep(x.npub)}>
            <div class="mod-check" class:on={kept} role="checkbox" aria-checked={String(kept)}>{#if kept}<span class="icon icon-check"></span>{/if}</div>
            <Avatar src={src(x)} size={30} class="mod-avatar" />
            <div class="mod-body">
                <div class="mod-row-top">
                    <span class="mod-name cutoff">{name(x)}</span>
                    {#if badge(x)}<span class="mod-badge mod-badge-{x.verdict}">{badge(x)}</span>{/if}
                </div>
                <!-- Tenure, not the raw join: a migration re-seeds every join at the same moment. -->
                <div class="mod-meta cutoff">
                    {#if x.tenure_secs}here <span class="mod-num">{h.ago(x.tenure_secs)}</span>{:else}<span class="mod-unknown">age unknown</span>{/if}<span class="mod-dot">·</span><span class="mod-num">{x.messages}</span> msg{x.messages === 1 ? '' : 's'}{#if x.distinct > 0}<span class="mod-dot">·</span><span class="mod-num">{x.distinct}</span> distinct{/if}{#if x.invite_label}<span class="mod-dot">·</span>via {x.invite_label}{/if}
                </div>
                {#if why.length}<div class="mod-why cutoff">{why.join(' · ')}</div>{/if}
            </div>
        </div>
    {/each}
{/if}
