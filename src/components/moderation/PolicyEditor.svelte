<script>
    // One draft: its dials, the preview of what it would have caught, and the gated Enable.
    import { polState, polDraft, polDirty, polShowGallery } from '../lib/policy.svelte.js';
    import PolicyDial from './PolicyDial.svelte';
    import PolicyPreview from './PolicyPreview.svelte';
    let { h } = $props();
    const st = polState();
    const draft = $derived(polDraft());
    // Judged on the COMPOSED policy, so a template whose boxes are legitimately empty still passes.
    const readiness = $derived(draft ? h.readiness() : { ok: false, reason: '' });
</script>

{#if draft}
<div class="pol-editor">
    <div class="pol-editor-head">
        <button class="pol-back" aria-label="Back" onclick={polShowGallery}>
            <span class="icon icon-chevron-double-left"></span>
        </button>
        <h4 class="pol-editor-title">{draft.name}</h4>
    </div>
    <p class="pol-caveat">{draft.caveat}</p>
    <div class="pol-dials">
        {#each draft.dials as d (d.key || d.kind)}
            <PolicyDial {d} {h} onchange={polDirty} />
        {/each}
    </div>

    {#if st.previewError}
        <div class="pol-preview-out"><div class="pol-warn"><span class="pol-warn-name">Not ready:</span> {st.previewError}</div></div>
    {:else if st.previewed}
        <PolicyPreview {h} />
    {/if}

    <div class="pol-editor-foot">
        <button class="mod-btn pol-preview-btn" class:is-busy={st.busy} disabled={st.busy || !readiness.ok}
                title={readiness.ok ? '' : readiness.reason} onclick={h.preview}>
            <span class="pol-spin" aria-hidden="true"></span>
            <span class="mod-btn-label">{st.busy ? 'Checking real history…' : 'Preview on real history'}</span>
        </button>
        <button class="mod-btn mod-btn-primary" disabled={!st.previewed} onclick={h.save}>
            <span class="mod-btn-label">{st.previewed ? 'Enable this policy' : 'Preview before Enabling'}</span>
        </button>
    </div>
</div>
{/if}
