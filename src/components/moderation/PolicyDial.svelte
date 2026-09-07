<script>
    // One dial of a draft. Edits write straight into the draft and report a change, which drops the preview.
    import { polState, polChannels, polDraft } from '../lib/policy.svelte.js';
    import PolicyRules from './PolicyRules.svelte';
    let { d, h, onchange } = $props();
    // Read from the store, not a prop: dial edits write into the draft in place.
    const draft = $derived(polDraft());
    const st = polState();
    const says = $derived((h.strictness.find(x => x.id === draft.strictness) || h.strictness[1]).says);
    const chosen = $derived(new Set(draft.values[d.key] || []));

    function setStrictness(id) { draft.strictness = id; onchange(); }
    function toggleChannel(id) {
        const next = new Set(chosen);
        if (next.has(id)) next.delete(id); else next.add(id);
        draft.values[d.key] = [...next];
        onchange();
    }
    function explain(e) { e.preventDefault(); e.stopPropagation(); h.explainStrictness(); }
</script>

<div class="pol-dial">
    <!-- svelte-ignore a11y_label_has_associated_control -->
    <label class="pol-dial-label">{d.label}{#if d.kind === 'strictness'}<span class="icon icon-info pol-info" role="button" tabindex="0" aria-label="What does this change?" onclick={explain} onkeydown={(e) => e.key === 'Enter' && explain(e)}></span>{/if}</label>
    <div class="pol-dial-hint">{d.hint}</div>
    {#if d.kind === 'summary'}
        <div class="pol-summary">
            {#each draft.summary as r}
                <div class="pol-sum" class:pol-sum-armed={r.armed}>
                    <div class="pol-sum-name">{r.label}{#if r.armed}<span class="pol-sum-tag">only after another rule fires</span>{/if}</div>
                    <div class="pol-sum-detail">{r.detail}</div>
                </div>
            {/each}
        </div>
    {:else if d.kind === 'strictness'}
        <div class="pol-seg">
            {#each h.strictness as s (s.id)}
                <button class="pol-seg-btn" class:active={draft.strictness === s.id} onclick={() => setStrictness(s.id)}>{s.label}</button>
            {/each}
        </div>
        <div class="pol-seg-says">{says}</div>
    {:else if d.kind === 'wordlist' || d.kind === 'domainlist'}
        <textarea rows="4" placeholder={d.kind === 'wordlist' ? 'one word per line' : 'example.com'}
                  bind:value={draft.values[d.key]} oninput={onchange}></textarea>
    {:else if d.kind === 'text'}
        <input type="text" class="pol-text" placeholder="Spoiler filter" bind:value={draft.values[d.key]} oninput={onchange}>
    {:else if d.kind === 'rules'}
        <PolicyRules {h} {onchange} />
    {:else if d.kind === 'seconds'}
        <div class="pol-seconds">
            <input type="number" min="1" max="3600" class="pol-text pol-seconds-input" bind:value={draft.values[d.key]} oninput={onchange}>
            <span class="pol-seconds-unit">seconds</span>
        </div>
    {:else if d.kind === 'channels'}
        <div class="pol-chips">
            {#each polChannels() as ch (ch.id)}
                <button class="pol-chip" class:on={chosen.has(ch.id)} onclick={() => toggleChannel(ch.id)}>#{ch.name}</button>
            {:else}
                <span class="pol-hint">{st.channelsLoaded ? 'This community has no channels.' : 'Loading channels…'}</span>
            {/each}
        </div>
    {/if}
</div>
