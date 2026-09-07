<script>
    // The from-scratch rule list. Every kind on offer can convict on its own; the catalogue
    // withholds the aggravators, which mean nothing unarmed.
    import { polRuleKinds, polDraft } from '../lib/policy.svelte.js';
    let { h, onchange } = $props();
    const draft = $derived(polDraft());
    const kinds = polRuleKinds();
    const rows = $derived((draft.values.rules || []).map((r, i) => ({ r, i, kind: kinds.find(k => k.id === r.kind) })).filter(x => x.kind));

    function add(k) {
        if (!draft.values.rules) draft.values.rules = [];
        draft.values.rules.push({ kind: k.id, value: '' });
        onchange();
    }
    function remove(i) { draft.values.rules.splice(i, 1); onchange(); }
</script>

<div class="pol-rules">
    {#each rows as { r, i, kind } (r)}
        <div class="pol-rule">
            <div class="pol-rule-head">
                <span class="pol-rule-name">{kind.label}</span>
                <button class="pol-rule-x" aria-label="Remove" onclick={() => remove(i)}>&#x2715;</button>
            </div>
            <div class="pol-rule-desc">{kind.description}</div>
            {#if kind.input !== 'none'}
                <div class="pol-rule-hint">{kind.input_hint}</div>
                {#if kind.input === 'seconds'}
                    <div class="pol-seconds">
                        <input type="number" min="1" max="3600" class="pol-text pol-seconds-input"
                               value={r.value || String(kind.rule?.match?.per_secs || 10)}
                               oninput={(e) => { r.value = e.currentTarget.value; onchange(); }}>
                        <span class="pol-seconds-unit">seconds</span>
                    </div>
                {:else}
                    <textarea rows="3" placeholder={kind.input === 'wordlist' ? 'one word per line' : 'example.com'}
                              bind:value={r.value} oninput={onchange}></textarea>
                {/if}
            {/if}
        </div>
    {/each}
    <div class="pol-rule-add">
        {#each kinds as k (k.id)}
            <button class="pol-rule-add-btn" title={k.description} onclick={() => add(k)}>+ {k.label}</button>
        {/each}
    </div>
</div>
