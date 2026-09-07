<script>
    // The shipped defaults run alongside anything the community writes, so they are shown
    // as a row rather than as the absence of one. When the community has forked them the
    // row says who replaced them: a badge claiming cover the engine is not providing is
    // worse than no badge.
    import { polState, polStored } from '../lib/policy.svelte.js';
    let { h } = $props();
    const st = polState();
    const rows = $derived(polStored().map(p => {
        const doc = h.parse(p.bytes);
        const rules = doc ? doc.rules.length : 0;
        return { ...p, name: doc?.name || p.policy_id, rules };
    }));
</script>

<div class="pol-list">
    {#if st.usingBuiltin}
        <div class="pol-row pol-row-builtin">
            <div class="pol-row-main">
                <div class="pol-row-name">Vector's defaults</div>
                <div class="pol-row-sub">Watching for raid waves &middot; running</div>
            </div>
            <button class="pol-inspect" onclick={h.inspectDefaults}>See the rules</button>
        </div>
    {:else}
        <div class="pol-row pol-row-builtin pol-row-forked">
            <div class="pol-row-main">
                <div class="pol-row-name">Vector's defaults</div>
                <div class="pol-row-sub">Replaced by your own version below.</div>
            </div>
        </div>
    {/if}
    {#each rows as p (p.policy_id)}
        <div class="pol-row">
            <div class="pol-row-main">
                <div class="pol-row-name">{p.name}</div>
                <div class="pol-row-sub" class:pol-row-invalid={!p.valid}>
                    {#if p.valid}{p.rules} rule{p.rules === 1 ? '' : 's'} · {p.enabled ? 'active' : 'paused'}{:else}Not running: {String(p.error || 'invalid')}{/if}
                </div>
            </div>
            <button class="pol-toggle" class:on={p.enabled} aria-label="Toggle" onclick={() => h.toggle(p)}></button>
            <button class="pol-delete" aria-label="Delete" onclick={() => h.remove(p.policy_id)}>&#x2715;</button>
        </div>
    {/each}
</div>
