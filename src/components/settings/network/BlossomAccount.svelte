<script>
    // What this media server will do for the signed-in account, in the server's own
    // words: the plan it puts the account on, the per-file limit, and the storage and
    // daily allowances with how much of each is used. Rendered only when the server
    // publishes a document.
    let { info, h } = $props();
    const caller = $derived(info.caller);
    // Server tiers are named for the policy that grants them; the person sees the plan.
    const PLANS = {
        badged: { name: 'Premium', kind: 'premium', blurb: 'Earned by early Vector supporters and bug hunters.' },
        recognised: { name: 'Free', kind: 'free', blurb: '' },
        default: { name: 'No access', kind: 'none', blurb: '' },
    };
    const plan = $derived.by(() => {
        if (!caller) return null;
        if (!caller.allowed) return PLANS.default;
        return PLANS[caller.tier] || { name: caller.tier || 'Member', kind: 'free', blurb: '' };
    });
    const serverLine = $derived.by(() => {
        const name = info.name || info.software || '';
        const version = info.version ? ` ${info.version}` : '';
        return name ? `${name}${version}` : '';
    });
    function pct(used, limit) {
        if (!limit) return 0;
        return Math.min(100, Math.round((used / limit) * 100));
    }
    // Past 90% the meter warns, at the limit it alarms.
    function tone(used, limit) {
        if (!limit) return '';
        if (used >= limit) return 'blossom-meter-full';
        if (used / limit >= 0.9) return 'blossom-meter-high';
        return '';
    }
</script>

{#snippet meter(label, used, limit, note)}
    <div class="blossom-meter">
        <div class="blossom-meter-head">
            <span class="blossom-meter-label">{label}</span>
            <span class="blossom-meter-value">{h.formatBytes(used, 1)} <span class="blossom-meter-of">of {h.formatBytes(limit, 1)}</span></span>
        </div>
        <div class="blossom-meter-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct(used, limit)} aria-label={label}>
            <div class="blossom-meter-fill {tone(used, limit)}" style="width: {pct(used, limit)}%"></div>
        </div>
        {#if note}<div class="blossom-meter-note">{note}</div>{/if}
    </div>
{/snippet}

{#if caller && plan}
    <div class="blossom-plan blossom-plan-{plan.kind}">
        <div class="blossom-plan-head">
            <div class="blossom-plan-title">
                <span class="blossom-plan-eyebrow">Your plan</span>
                <span class="blossom-plan-name">{plan.name}</span>
                {#if plan.blurb}<span class="blossom-plan-blurb">{plan.blurb}</span>{/if}
            </div>
            {#if caller.allowed}
                <div class="blossom-plan-perfile">
                    <span class="blossom-plan-perfile-value">{h.formatBytes(caller.max_blob, 0)}</span>
                    <span class="blossom-plan-perfile-label">per file</span>
                </div>
            {/if}
        </div>
        {#if caller.allowed}
            <div class="blossom-plan-meters">
                {@render meter('Storage', caller.storage_used, caller.storage_limit, `${caller.blobs} file${caller.blobs === 1 ? '' : 's'} stored`)}
                {@render meter('Today', caller.daily_used, caller.daily_limit, 'Resets at midnight UTC')}
            </div>
        {:else}
            <p class="blossom-plan-refused">
                {#if caller.reasons?.length}{caller.reasons.join(' ')}{:else}This server won’t take uploads from your account.{/if}
            </p>
        {/if}
    </div>
{/if}
{#if serverLine || info.description || (!caller && info.max_blob)}
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
