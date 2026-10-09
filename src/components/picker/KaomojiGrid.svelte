<script>
    // The kaomoji view: recents, then each category under its header, with a strip to jump
    // between them; a query swaps in the matches, ranked by name, tags, category, then face.
    import { KAOMOJI } from '../lib/kaomoji.js';
    import { pickerState } from '../lib/picker.svelte.js';

    let { h } = $props();   // h: pick(face, event)

    const st = pickerState();
    const RECENTS_KEY = 'vector:kaomoji-recents';
    const RECENTS_CAP = 18;

    let list = $state(null);
    let strip = $state(null);
    let recents = $state(load());
    let current = $state(0);                 // the category the list is scrolled to
    let edges = $state({ start: true, end: false });

    function load() {
        try {
            const saved = JSON.parse(localStorage.getItem(RECENTS_KEY) || '[]');
            return Array.isArray(saved) ? saved.filter((f) => typeof f === 'string').slice(0, RECENTS_CAP) : [];
        } catch (_) {
            return [];
        }
    }

    const byFace = new Map();
    for (const cat of KAOMOJI) {
        for (const [face, name, tags] of cat.items) {
            if (!byFace.has(face)) byFace.set(face, { face, name, tags: tags.split(' '), category: cat.name.toLowerCase() });
        }
    }

    const query = $derived(st.query.trim().toLowerCase());
    const matches = $derived.by(() => {
        if (!query) return [];
        const rank = (k) => {
            const name = k.name.toLowerCase();
            if (name.startsWith(query)) return 0;
            if (k.tags.some((t) => t.startsWith(query))) return 1;
            if (name.includes(query)) return 2;
            if (k.category.startsWith(query)) return 3;
            if (k.face.includes(query)) return 4;
            return -1;
        };
        return [...byFace.values()]
            .map((k) => [rank(k), k])
            .filter(([r]) => r >= 0)
            .sort((a, b) => a[0] - b[0])
            .map(([, k]) => k);
    });
    const recentItems = $derived(recents.map((f) => byFace.get(f)).filter(Boolean));

    function pick(face, e) {
        recents = [face, ...recents.filter((f) => f !== face)].slice(0, RECENTS_CAP);
        try { localStorage.setItem(RECENTS_KEY, JSON.stringify(recents)); } catch (_) { /* recents are a convenience */ }
        h.pick(face, e);
    }

    function jump(i) {
        list?.querySelector(`[data-kaomoji-cat="${i}"]`)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
    }

    // A mouse wheel only scrolls down: over the strip, down is sideways.
    function wheel(e) {
        if (!strip || Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
        e.preventDefault();
        strip.scrollLeft += e.deltaY;
    }

    function stripScrolled() {
        if (!strip) return;
        edges = { start: strip.scrollLeft <= 1, end: strip.scrollLeft + strip.clientWidth >= strip.scrollWidth - 1 };
    }

    // The strip follows the list: the category under the top edge lights up and slides into view.
    function listScrolled() {
        if (!list || query) return;
        const top = list.getBoundingClientRect().top + 8;
        let at = 0;
        for (const header of list.querySelectorAll('[data-kaomoji-cat]')) {
            if (header.getBoundingClientRect().top > top) break;
            at = Number(header.dataset.kaomojiCat);
        }
        if (at === current) return;
        current = at;
        strip?.querySelector(`[data-cat="${at}"]`)?.scrollIntoView({ inline: 'center', block: 'nearest' });
    }

    $effect(() => {
        if (!strip) return;
        strip.addEventListener('wheel', wheel, { passive: false });
        stripScrolled();
        return () => strip.removeEventListener('wheel', wheel);
    });
</script>

{#snippet items(entries)}
    <div class="kaomoji-items">
        {#each entries as k (k.face)}
            <button class="kaomoji-item" title={k.name} onclick={(e) => pick(k.face, e)}>{k.face}</button>
        {/each}
    </div>
{/snippet}

{#if !query}
    <div class="kaomoji-cats" class:fade-start={!edges.start} class:fade-end={!edges.end} bind:this={strip} onscroll={stripScrolled}>
        {#each KAOMOJI as cat, i (cat.name)}
            <button class="kaomoji-cat" class:active={i === current} data-cat={i} onclick={() => jump(i)}>{cat.name}</button>
        {/each}
    </div>
{/if}
<div class="kaomoji-list" bind:this={list} onscroll={listScrolled}>
    {#if query}
        {#if matches.length}
            {@render items(matches)}
        {:else}
            <div class="kaomoji-empty">No kaomoji for “{st.query.trim()}”</div>
        {/if}
    {:else}
        {#if recentItems.length}
            <div class="emoji-section-header"><span class="section-icon icon icon-clock"></span><span class="header-text">Recently Used</span></div>
            {@render items(recentItems)}
        {/if}
        {#each KAOMOJI as cat, i (cat.name)}
            <div class="emoji-section-header" data-kaomoji-cat={i}><span class="header-text">{cat.name}</span></div>
            {@render items(cat.items.map(([face, name]) => ({ face, name })))}
        {/each}
    {/if}
</div>
