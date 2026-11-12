<script>
    // A message's revisions as a timeline, newest first: each edit is what it changed against
    // the version before it, folded to the change; the original is a short preview. A click
    // on a revision opens it whole. Text renders through the chat's own pipeline.
    import { tick } from 'svelte';
    import { editHistoryState } from '../lib/edithistory.svelte.js';
    import { diffWords, foldUnchanged } from '../lib/textdiff.js';
    let { h } = $props();
    const st = editHistoryState();

    const revisions = $derived.by(() => {
        const list = st.entries;
        const out = [];
        for (let i = list.length - 1; i >= 0; i--) {
            out.push({ i, entry: list[i], ops: i > 0 ? diffWords(list[i - 1].content || '', list[i].content || '') : null });
        }
        return out;
    });
    let open = $state({});
    $effect(() => { st.msgId; open = {}; });

    function when(ts) {
        return new Date(ts).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
    }

    // A line's block marker stays outside a change, so a changed heading or quote still parses.
    const BLOCK = /^[ \t]*(?:#{1,6}[ \t]|>[ \t]?|[-*+][ \t]|\d+[.)][ \t])/;
    /** The diff as one source for the renderer: changes and folds marked with sentinels. */
    function diffSource(ops) {
        const m = h.marks;
        let src = '';
        const gaps = [];
        for (const r of ops) {
            if (r.op === 'gap') { src += r.lines ? `\n${m.gap}\n` : ` ${m.gap} `; gaps.push(r.words); continue; }
            if (r.op === 'eq') { src += r.text; continue; }
            const atLine = src === '' || src.endsWith('\n');
            const lead = atLine ? (r.text.match(BLOCK)?.[0] || '') : '';
            src += lead + (r.op === 'ins' ? m.ins : m.del) + r.text.slice(lead.length) + m.end;
        }
        return { src, gaps };
    }

    // Only a preview that is cut short fades, and only it opens on a click.
    function render(node, [source, tags, gaps]) {
        const draw = (s, t, g) => {
            h.renderText(node, s, t, g);
            requestAnimationFrame(() => node.classList.toggle('overflows', node.scrollHeight > node.clientHeight + 1));
        };
        draw(source, tags, gaps);
        return { update([s, t, g]) { draw(s, t, g); } };
    }
    function toggle(e, i) {
        // A link inside still works as a link.
        if (e.target.closest('a')) return;
        e.stopPropagation();
        const box = e.currentTarget;
        if (box.classList.contains('collapsed') && !box.classList.contains('overflows')) return;
        open = { ...open, [i]: !open[i] };
        // Folding a long revision shrinks the list under the reader: keep the one clicked in view.
        const rev = e.currentTarget.closest('.eh-rev');
        tick().then(() => rev?.scrollIntoView({ block: 'nearest' }));
    }
</script>

<ol class="eh-timeline">
    {#each revisions as rev (rev.i)}
        {@const expanded = !!open[rev.i]}
        <li class="eh-rev" class:current={rev.i === st.entries.length - 1}>
            <div class="eh-rev-head">{rev.i === 0 ? 'Original' : 'Edited'} <span>{when(rev.entry.edited_at)}</span></div>
            {#if rev.ops}
                {@const d = diffSource(expanded ? rev.ops : foldUnchanged(rev.ops, 6))}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="eh-text eh-diff dmsg-text" onclick={(e) => toggle(e, rev.i)} use:render={[d.src, st.tags, d.gaps]}></div>
            {:else}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="eh-text dmsg-text" class:collapsed={!expanded} onclick={(e) => toggle(e, rev.i)} use:render={[rev.entry.content || '', st.tags, null]}></div>
            {/if}
        </li>
    {/each}
</ol>
