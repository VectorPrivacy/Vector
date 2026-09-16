<script>
    // What this media server will do for the signed-in account, in the server's own
    // words: the plan it puts the account on, the per-file limit, and the storage and
    // daily allowances with how much of each is used. Rendered only when the server
    // publishes a document.
    let { info, host = '', h } = $props();
    const caller = $derived(info.caller);
    // The server names the plan (already bounded by the backend); Vector only decides
    // the treatment. The premium effects are a statement of trust, so they are reserved
    // for Vector's own servers — any other server saying "Premium" gets a plain card.
    const OFFICIAL_DOMAINS = ['vectorapp.io', 'jskitty.com'];
    const official = $derived(OFFICIAL_DOMAINS.some(d => host === d || host.endsWith('.' + d)));
    const plan = $derived.by(() => {
        if (!caller) return null;
        if (!caller.allowed) return { name: 'No access', kind: 'none', blurb: '' };
        const name = caller.plan_title || caller.tier || 'Member';
        const kind = official && caller.tier === 'badged' ? 'premium' : 'free';
        return { name, kind, blurb: caller.plan_description || '' };
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
            <span class="blossom-meter-label">{label}{#if note}<span class="blossom-meter-note">{' · '}{note}</span>{/if}</span>
            <span class="blossom-meter-value">{h.formatBytes(used, 1)} <span class="blossom-meter-of">of {h.formatBytes(limit, 1)}</span></span>
        </div>
        <div class="blossom-meter-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct(used, limit)} aria-label={label}>
            <div class="blossom-meter-fill {tone(used, limit)}" style="width: {pct(used, limit)}%"></div>
        </div>
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
                {@render meter('Storage', caller.storage_used, caller.storage_limit, `${caller.blobs} file${caller.blobs === 1 ? '' : 's'}`)}
                {@render meter('Today', caller.daily_used, caller.daily_limit, 'resets 00:00 UTC')}
            </div>
        {:else}
            <p class="blossom-plan-refused">
                {#if caller.reasons?.length}{caller.reasons.join(' ')}{:else}This server won’t take uploads from your account.{/if}
            </p>
        {/if}
    </div>
{/if}
{#if !caller && info.max_blob}
    <p class="blossom-cap-blurb">Accepts files up to {h.formatBytes(info.max_blob, 1)}.</p>
{/if}

