<script>
    // What this media server will do for the signed-in account, in the server's own
    // words: its tier, the per-file limit, and the storage and daily allowances with
    // how much of each is used. Rendered only when the server publishes a document.
    let { info, h } = $props();
    const caller = $derived(info.caller);
    const serverLine = $derived.by(() => {
        const name = info.name || info.software || '';
        const version = info.version ? ` ${info.version}` : '';
        return name ? `${name}${version}` : '';
    });
    function pct(used, limit) {
        if (!limit) return 0;
        return Math.min(100, Math.round((used / limit) * 100));
    }
    // Past 90% the bar warns, at the limit it alarms.
    function tone(used, limit) {
        if (!limit) return '';
        if (used >= limit) return 'blossom-quota-full';
        if (used / limit >= 0.9) return 'blossom-quota-high';
        return '';
    }
</script>

{#snippet quota(label, used, limit, note)}
    <div class="blossom-quota">
        <div class="blossom-quota-head">
            <span class="blossom-quota-label">{label}</span>
            <span class="blossom-quota-value">{h.formatBytes(used, 1)} <span class="blossom-quota-of">of {h.formatBytes(limit, 1)}</span></span>
        </div>
        <div class="blossom-quota-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct(used, limit)} aria-label={label}>
            <div class="blossom-quota-fill {tone(used, limit)}" style="width: {pct(used, limit)}%"></div>
        </div>
        {#if note}<div class="blossom-quota-note">{note}</div>{/if}
    </div>
{/snippet}

{#if caller}
    <div class="relay-metrics-section">
        <div class="relay-metrics-header">
            <h4>Your account</h4>
            <span class="relay-status relay-status-small" class:connected={caller.allowed} class:disconnected={!caller.allowed}>{caller.tier || (caller.allowed ? 'allowed' : 'refused')}</span>
        </div>
        {#if !caller.allowed}
            <p class="blossom-account-refused">
                {#if caller.reasons?.length}{caller.reasons.join(' ')}{:else}This server won't take uploads from your account.{/if}
            </p>
        {:else}
            <div class="blossom-quota-grid">
                <div class="blossom-quota blossom-quota-inline">
                    <span class="blossom-quota-label">Per file</span>
                    <span class="blossom-quota-value">up to {h.formatBytes(caller.max_blob, 1)}</span>
                </div>
                {@render quota('Storage', caller.storage_used, caller.storage_limit, `${caller.blobs} file${caller.blobs === 1 ? '' : 's'} stored`)}
                {@render quota('Today', caller.daily_used, caller.daily_limit, 'Resets at midnight UTC')}
            </div>
        {/if}
    </div>
{/if}
{#if serverLine || info.description}
    <div class="relay-metrics-section">
        <div class="relay-metrics-header">
            <h4>Server</h4>
            {#if serverLine}<span class="blossom-server-line">{serverLine}</span>{/if}
        </div>
        {#if info.description}<p class="blossom-cap-blurb">{info.description}</p>{/if}
        {#if !caller && info.max_blob}
            <p class="blossom-cap-blurb">Accepts files up to {h.formatBytes(info.max_blob, 1)}.</p>
        {/if}
    </div>
{/if}
