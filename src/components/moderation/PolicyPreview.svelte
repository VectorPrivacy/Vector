<script>
    // What the draft would have flagged over the last week. Naming the corpus separates "your
    // rule caught nobody" from "there was nothing to catch"; the shielded count is the number
    // the flagged list cannot show: regulars who tripped the wire and were spared by standing.
    import { polPreview } from '../lib/policy.svelte.js';
    let { h } = $props();
    const res = $derived(polPreview());
    const n = $derived(res.flagged.length);
    const shielded = $derived(res.shielded_matches || []);
    const num = (x) => (x || 0).toLocaleString();
    const plural = (x, w) => `${w}${x === 1 ? '' : 's'}`;
</script>

<div class="pol-preview-out">
    <div class="pol-preview-head">
        Over the last 7 days this would have flagged
        <span class="pol-preview-num">{n}</span> {plural(n, 'member')}
        and cited <span class="pol-preview-num">{res.messages_cited}</span> {plural(res.messages_cited, 'message')}.
    </div>
    {#if !n}
        <div class="pol-hint">Checked against {num(res.corpus)} {plural(res.corpus, 'message')},
            and nothing tripped this rule. Usually good news, though it also means the preview cannot tell you
            whether the rule works. Try a word you know someone has used.</div>
    {/if}
    {#if shielded.length}
        <div class="pol-warn">{shielded.length} trusted {plural(shielded.length, 'member')} also matched this rule and
            {shielded.length === 1 ? 'was' : 'were'} spared only by their standing:
            {#each shielded.slice(0, 4) as r, i}{#if i}{', '}{/if}<span class="pol-warn-name">{h.name(r.npub)}</span> ({r.messages} msgs){/each}{#if shielded.length > 4}, …{/if}.
            If this rule is meant for raiders, it is catching ordinary conversation.</div>
    {/if}
    {#if res.unevaluated?.length}
        <div class="pol-hint" style="margin-top:8px;">Not checked here: {res.unevaluated.join(', ')}</div>
    {/if}
    {#if n}
        <div class="pol-preview-rows">
            {#each res.flagged.slice(0, 30) as r (r.npub)}
                <div class="pol-preview-row">
                    <span class="pol-preview-row-name">{h.name(r.npub)}</span>
                    <span class="pol-preview-row-why">{(r.reasons || [])[0] || ''}</span>
                    <span class="pol-preview-score">{r.score}{r.proven === 0 ? ' · suspected' : ' · provable'}</span>
                </div>
            {/each}
        </div>
    {/if}
</div>
