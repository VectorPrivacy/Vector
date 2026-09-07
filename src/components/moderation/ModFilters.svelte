<script>
    // Everyone / Removing / Staying, with live counts. The two groups always sum to Everyone.
    import { modState, modIntel, modKeep, modSetFilter } from '../lib/moderation.svelte.js';
    const st = modState();
    const FILTERS = [{ id: 'all', label: 'Everyone' }, { id: 'cut', label: 'Removing' }, { id: 'keep', label: 'Staying' }];
    const counts = $derived.by(() => {
        const intel = modIntel(); const keep = modKeep();
        if (!intel) return null;
        const all = intel.report.members.length;
        const cut = intel.report.members.filter(x => !keep.has(x.npub)).length;
        return { all, cut, keep: all - cut };
    });
</script>

{#each FILTERS as f (f.id)}
    <button class="mod-chip" class:active={st.filter === f.id} onclick={() => modSetFilter(f.id)}>{f.label}{#if counts}<span class="mod-chip-count">{counts[f.id]}</span>{/if}</button>
{/each}
