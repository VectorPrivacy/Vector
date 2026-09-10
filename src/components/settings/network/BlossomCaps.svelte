<script>
    // What a media server has accepted and refused, learned from real uploads: accepted types
    // with their largest size, size-limited types, and rejected types, each tagged with the
    // context it was tested in.
    import { blossomCapsState } from '../../lib/settings.svelte.js';
    let { h } = $props();
    const st = blossomCapsState();
    const caps = $derived(st.caps || []);
    // outcome 1 = accepted, 2 = MIME rejected, 3 = size-only seed.
    const accepted = $derived(caps.filter(c => c.outcome === 1 && c.max_accepted_size > 0));
    const limited = $derived(caps.filter(c => (c.outcome === 3 && c.min_rejected_size != null)
        || (c.outcome === 1 && c.max_accepted_size === 0 && c.min_rejected_size != null)));
    const rejected = $derived(caps.filter(c => c.outcome === 2));
</script>

{#snippet context(c)}
    <span class="blossom-cap-context" title={c.is_encrypted ? 'Tested with encrypted chat data' : 'Tested with public uploads (avatar, banner, etc.)'}>{c.is_encrypted ? 'encrypted' : 'public'}</span>
{/snippet}

{#if st.status === 'loading'}
    <span style="opacity: 0.6;">Loading…</span>
{:else if st.status === 'error'}
    <span style="opacity: 0.6;">Could not load capability data.</span>
{:else if !caps.length}
    <span style="opacity: 0.6;">No capability data yet. Vector learns each server’s file-type and size limits as you send files.</span>
{:else}
    {#if accepted.length}
        <div class="blossom-cap-group-label blossom-cap-accepted">Accepts</div>
        <ul class="blossom-cap-list">
            {#each accepted as c}
                <li><span class="blossom-cap-marker blossom-cap-accepted" aria-label="accepted">✓</span><span class="blossom-cap-mime">{c.mime_type}</span>{@render context(c)}<span class="blossom-cap-size">{h.formatBytes(c.max_accepted_size, 1)} max</span></li>
            {/each}
        </ul>
    {/if}
    {#if limited.length}
        <div class="blossom-cap-group-label blossom-cap-limited">Size-limited</div>
        <ul class="blossom-cap-list">
            {#each limited as c}
                <li class="blossom-cap-limited-row"><span class="blossom-cap-marker blossom-cap-limited" aria-label="size limited">⚠</span><span class="blossom-cap-mime">{c.mime_type}</span>{@render context(c)}<span class="blossom-cap-size">rejects ≥ {h.formatBytes(c.min_rejected_size, 1)}</span></li>
            {/each}
        </ul>
    {/if}
    {#if rejected.length}
        <div class="blossom-cap-group-label blossom-cap-rejected">Rejects</div>
        <ul class="blossom-cap-list">
            {#each rejected as c}
                <li class="blossom-cap-rejected-row"><span class="blossom-cap-mime">{c.mime_type}</span>{@render context(c)}<span class="blossom-cap-marker blossom-cap-rejected" aria-label="rejected">✕</span></li>
            {/each}
        </ul>
    {/if}
{/if}
