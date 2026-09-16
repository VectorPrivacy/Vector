<script>
    // One media server: whether it is enabled, what it will do for this account, and
    // remove (custom) or enable / disable (default). A server that publishes its own
    // document speaks for itself; otherwise the view is what uploads have taught us.
    import { blossomInfoDialog, blossomInfoState, blossomStatsState } from '../../lib/network.svelte.js';
    import BlossomCaps from './BlossomCaps.svelte';
    import BlossomAccount from './BlossomAccount.svelte';
    import BlossomPerformance from './BlossomPerformance.svelte';
    let { h } = $props();   // h: close(), action(), formatBytes
    const st = blossomInfoDialog.state();
    const doc = blossomInfoState();
    const perf = blossomStatsState();
    // The list already shows the state; here it appears only when something is wrong.
    const NOTICES = {
        offline: 'Vector couldn’t reach this server recently. Uploads are routed elsewhere until it answers again.',
        maxed: 'An allowance on this server is used up. Uploads are routed elsewhere until it frees up.',
        no_access: 'This server won’t take uploads from your account.',
        disabled: 'Disabled — uploads aren’t routed here.',
    };
    const notice = $derived.by(() => {
        const state = st.status?.state || (st.enabled ? 'online' : 'disabled');
        const text = NOTICES[state];
        return text ? { label: st.status?.label || 'Disabled', tone: st.status?.tone || 'disabled', text } : null;
    });
    // The dialog is the server; the verb alone fits a phone-width button.
    const actionLabel = $derived(st.isCustom ? 'Remove' : (st.enabled ? 'Disable' : 'Enable'));
    const personalised = $derived(doc.status === 'ok' && doc.info && doc.info.caller);
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="relay-dialog-overlay" class:active={st.active}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog relay-info-dialog">
            <div class="relay-dialog-content blossom-dialog-content">
                <div class="blossom-dialog-host">{st.url}</div>
                {#if notice}
                    <div class="blossom-notice blossom-notice-{notice.tone}">
                        <span class="relay-status relay-status-small {notice.tone}">{notice.label}</span>
                        <span class="blossom-notice-text">{notice.text}</span>
                    </div>
                {/if}
                {#if perf.stats}
                    <BlossomPerformance stats={perf.stats} {h} />
                {/if}
                {#if doc.status === 'loading'}
                    <!-- The loaded layout's shape, so the answer changes text, not geometry. -->
                    <div class="blossom-plan blossom-plan-skeleton" aria-busy="true">
                        <div class="blossom-plan-head">
                            <div class="blossom-plan-title">
                                <span class="blossom-plan-eyebrow">Your plan</span>
                                <span class="blossom-plan-name">&nbsp;</span>
                            </div>
                            <div class="blossom-plan-perfile">
                                <span class="blossom-plan-perfile-value">&nbsp;</span>
                                <span class="blossom-plan-perfile-label">per file</span>
                            </div>
                        </div>
                        <div class="blossom-plan-meters">
                            <div class="blossom-meter">
                                <div class="blossom-meter-head"><span class="blossom-meter-label">Storage</span><span class="blossom-meter-value">&nbsp;</span></div>
                                <div class="blossom-meter-track"></div>
                            </div>
                            <div class="blossom-meter">
                                <div class="blossom-meter-head"><span class="blossom-meter-label">Today</span><span class="blossom-meter-value">&nbsp;</span></div>
                                <div class="blossom-meter-track"></div>
                            </div>
                        </div>
                    </div>
                {:else if personalised}
                    <BlossomAccount info={doc.info} host={st.url} {h} />
                {:else}
                    {#if doc.status === 'ok' && doc.info}
                        <BlossomAccount info={doc.info} host={st.url} {h} />
                    {/if}
                    <div class="relay-metrics-section">
                        <div class="relay-metrics-header">
                            <h4>What we’ve learned</h4>
                        </div>
                        <p class="blossom-cap-blurb">
                            This server doesn’t say what it accepts, so Vector learns as you send: the largest file it has taken of each type, and the types it has refused. Uploads are routed to the best-suited server automatically.
                        </p>
                        <div class="blossom-cap-slot"><BlossomCaps {h} /></div>
                    </div>
                {/if}
                <div class="relay-dialog-buttons">
                    <button class="btn danger-btn" onclick={h.action}>{actionLabel}</button>
                    <button class="btn primary-btn" onclick={h.close}>Done</button>
                </div>
            </div>
        </div>
    </div>
{/if}
