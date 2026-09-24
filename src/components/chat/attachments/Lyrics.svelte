<script>
    // Lyrics that follow the music. Timed lyrics keep the sung line bright near the top of
    // the view with the rest dimmed, softening with distance while the music plays; words
    // fill in as they are sung when the file times them, and a tap on a line plays from
    // there. Plain lyrics are a calm scrolling text.
    let { lyrics, positionMs, playing, onSeek } = $props();
    // lyrics: { synced, lines: [{ at_ms, text, words: [{ at_ms, text }] }] } (src-tauri lyrics.rs)

    const lines = $derived(lyrics.lines);
    const current = $derived.by(() => {
        if (!lyrics.synced) return -1;
        let idx = -1;
        for (let i = 0; i < lines.length; i++) {
            if (lines[i].at_ms <= positionMs) idx = i;
            else break;
        }
        return idx;
    });

    // How lit each word is, 0 to 1. Sung lines are lit and upcoming ones are not; the sung
    // line fills word by word from each stamp to the next. The pieces never change between
    // states, so a line moving on only fades.
    function fill(line, i, w) {
        if (i < current) return 1;
        if (i > current) return 0;
        const end = line.words[w + 1]?.at_ms ?? lines[i + 1]?.at_ms ?? line.words[w].at_ms + 600;
        const span = Math.max(1, Math.min(end, line.words[w].at_ms + 1500) - line.words[w].at_ms);
        return Math.max(0, Math.min(1, (positionMs - line.words[w].at_ms) / span));
    }

    // ── following the song, unless the reader has taken over the scroll ──
    let scroller = $state(null);
    const lineEls = [];
    function lineEl(node, i) { lineEls[i] = node; return { destroy() { if (lineEls[i] === node) lineEls[i] = undefined; } }; }
    // Which edges have lines beyond them, for the fades.
    let moreAbove = $state(false), moreBelow = $state(false);
    function edges() {
        if (!scroller) return;
        moreAbove = scroller.scrollTop > 1;
        moreBelow = scroller.scrollTop + scroller.clientHeight < scroller.scrollHeight - 1;
    }
    // The line at the top of the view and how far into it the view sits, kept current on
    // every scroll: a new width rewraps every line, and this is what the view returns to.
    let anchor = { i: 0, offset: 0 };
    function noteAnchor() {
        if (!scroller) return;
        const top = scroller.scrollTop;
        const i = lineEls.findIndex((el) => el && el.offsetTop + el.offsetHeight > top);
        if (i >= 0) anchor = { i, offset: top - lineEls[i].offsetTop };
    }
    function onScroll() { edges(); noteAnchor(); }
    // A width change (the pop-out resized, the window narrowed) rewraps the lines under a
    // scroll offset that no longer means the same place. Following the song, the sung line
    // is put straight back in its spot; read by hand, the line that was on top stays there.
    let lastWidth = 0;
    function rewrapped() {
        if (glide) { cancelAnimationFrame(glide); glide = null; }
        const el = lineEls[current];
        if (current >= 0 && el && performance.now() >= heldUntil) {
            scroller.scrollTop = el.offsetTop - scroller.clientHeight * 0.3;
        } else if (lineEls[anchor.i]) {
            scroller.scrollTop = lineEls[anchor.i].offsetTop + anchor.offset;
        }
    }
    $effect(() => {
        if (!scroller) return;
        const ro = new ResizeObserver(() => {
            const w = scroller.clientWidth;
            if (lastWidth && w !== lastWidth) rewrapped();
            lastWidth = w;
            edges();
        });
        ro.observe(scroller);
        edges();
        return () => ro.disconnect();
    });
    let heldUntil = 0;
    function hold() {
        heldUntil = performance.now() + 3000;
        if (glide) { cancelAnimationFrame(glide); glide = null; }
    }
    // A glide of our own: the browser's smooth scroll is short and abrupt next to a line
    // fading in, and cannot be retargeted mid-flight without a jolt.
    let glide = null;
    function glideTo(top) {
        if (glide) cancelAnimationFrame(glide);
        const from = scroller.scrollTop, to = Math.max(0, Math.min(top, scroller.scrollHeight - scroller.clientHeight));
        const start = performance.now(), dur = 650;
        const ease = (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
        const step = (now) => {
            const x = Math.min(1, (now - start) / dur);
            scroller.scrollTop = from + (to - from) * ease(x);
            glide = x < 1 ? requestAnimationFrame(step) : null;
        };
        glide = requestAnimationFrame(step);
    }
    $effect(() => () => { if (glide) cancelAnimationFrame(glide); });

    // A tapped line is where the song goes: the view follows it there at once, even out of
    // a scroll the reader was holding.
    function jump(i) {
        heldUntil = 0;
        const el = lineEls[i];
        if (el) glideTo(el.offsetTop - scroller.clientHeight * 0.3);
        onSeek(lines[i].at_ms);
    }

    // New lines (the next song of an album) land on their line at once, never glide from the
    // old song's place. The sheet is kept rather than remade: remaking it empties the panel
    // for a moment, and the chat around it jumps.
    let placed = false;
    let shownLines = null;
    $effect.pre(() => {
        if (lines !== shownLines) { shownLines = lines; placed = false; }
    });
    $effect(() => {
        const i = current;
        lines;   // a new song's lines re-place the view even at the same line number
        if (!scroller || performance.now() < heldUntil) return;
        edges();
        // Before the first line (a finished song, a seek to the start): back to the top.
        if (i < 0) { if (placed) glideTo(0); return; }
        const el = lineEls[i];
        if (!el) return;
        const top = el.offsetTop - scroller.clientHeight * 0.3;
        // The first placement lands; after that the view glides along with the song.
        if (placed) glideTo(top);
        else scroller.scrollTop = top;
        placed = true;
    });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="lyrics" class:is-synced={lyrics.synced} class:is-playing={playing} class:more-above={moreAbove} class:more-below={moreBelow}
     bind:this={scroller} onwheel={hold} ontouchmove={hold} onscroll={onScroll}>
    {#each lines as line, i (i)}
        {#if lyrics.synced}
            {@const d = current < 0 ? 0 : Math.abs(i - current)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="lyrics-line" class:is-current={i === current} class:is-past={i < current} class:is-break={!line.text}
                 style:--blur={playing && i !== current ? `${Math.min(d, 4) * 0.55}px` : '0px'}
                 use:lineEl={i} onclick={() => jump(i)}>
                {#if !line.text}
                    <span class="lyrics-dots"><span></span><span></span><span></span></span>
                {:else if line.words.length}
                    {#each line.words as word, w (w)}<span class="lyrics-word" style:--fill={fill(line, i, w)}>{word.text}</span>{' '}{/each}
                {:else}
                    <!-- No word timings: the line fills as one, eased by the stylesheet. -->
                    <span class="lyrics-word is-whole" style:--fill={i <= current ? 1 : 0}>{line.text}</span>
                {/if}
            </div>
        {:else}
            <div class="lyrics-line" class:is-break={!line.text}>{line.text || ' '}</div>
        {/if}
    {/each}
</div>
