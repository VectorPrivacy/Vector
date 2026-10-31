<script>
    // What the panel says before you read a row: members, trusted, staff, flagged.
    import { modIntel } from '../lib/moderation.svelte.js';
    const cells = $derived.by(() => {
        const intel = modIntel();
        if (!intel) return [];
        const r = intel.report;
        const flagged = r.members.filter(x => x.verdict === 'suspect').length;
        // Someone only seen posting is assessed but never counted, as in every member count.
        const unlisted = new Set(intel.unlisted);
        return [
            { n: r.members.filter(x => !unlisted.has(x.npub)).length, label: 'members', tone: 'quiet' },
            { n: r.trusted || 0, label: 'trusted', tone: 'good' },
            { n: r.protected || 0, label: 'staff', tone: 'staff' },
            { n: flagged, label: 'flagged', tone: flagged ? 'bad' : 'quiet' },
        ];
    });
</script>

{#each cells as c (c.label)}
    <div class="mod-stat mod-stat-{c.tone}"><span class="mod-stat-n">{c.n}</span><span class="mod-stat-l">{c.label}</span></div>
{/each}
