<script>
    // An album's songs: the number (live bars on the one playing), the title with its
    // edition note dimmed, the length. A tap plays that song.
    import { splitTitle } from '../../lib/audio.svelte.js';

    let { tracks, current = -1, playing = false, limit = 0, formatTime, onPick } = $props();
    // tracks: [{ title, lengthMs }]; limit: how many show until "Show all" (0 = every one)

    let showAll = $state(false);
    const shown = $derived(limit && !showAll ? Math.min(limit, tracks.length) : tracks.length);
</script>

<ol class="album-tracks">
    {#each tracks.slice(0, shown) as t, i (i)}
        {@const name = splitTitle(t.title)}
        <li>
            <button class="album-track" class:is-current={i === current} onclick={() => onPick(i)}>
                <span class="album-track-no">{#if i === current && playing}<span class="album-eq"><span></span><span></span><span></span></span>{:else}{i + 1}{/if}</span>
                <span class="album-track-title cutoff">{name.main}{#if name.note}<span class="title-note">{name.note}</span>{/if}</span>
                <span class="album-track-len">{formatTime(t.lengthMs / 1000)}</span>
            </button>
        </li>
    {/each}
    {#if limit && tracks.length > limit}
        <li><button class="album-more" onclick={() => (showAll = !showAll)}>{showAll ? 'Show fewer' : `Show all ${tracks.length} songs`}</button></li>
    {/if}
</ol>
