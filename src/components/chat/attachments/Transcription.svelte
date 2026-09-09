<script>
    // A voice message's transcription: clickable sections that seek, the one under the
    // playhead lit, the original language when translating. `open` slides the box in and
    // out (height, padding, margin and border together, the list scrolled to keep the
    // row in place); the sections blur in with a stagger on every open.
    let { t, positionMs, playing, h, onSeek } = $props();
    // t: { phase: 'ready' | 'error', sections: [{ at, text }], lang, error, open }; h: autoTranslate(), flag(lang), twemojify(el), scrollBy(px)

    let box = $state(null);
    let hidden = $state(true);
    let sections = [];
    let raf = null;

    const currentIdx = $derived.by(() => {
        if (!playing || !t.sections.length) return -1;
        for (let i = 0; i < t.sections.length; i++) {
            const start = parseFloat(t.sections[i].at);
            const next = i < t.sections.length - 1 ? parseFloat(t.sections[i + 1].at) : Infinity;
            if (positionMs >= start && positionMs < next) return i;
        }
        return -1;
    });

    const ease = (x) => 1 - Math.pow(1 - x, 3);

    function section(node, i) { sections[i] = node; return { destroy() { if (sections[i] === node) sections[i] = undefined; } }; }
    function revealSections() {
        const els = sections.filter(Boolean);
        for (const el of els) { el.style.transition = 'none'; el.style.opacity = '0'; el.style.filter = 'blur(4px)'; }
        void box.offsetHeight;
        els.forEach((el, i) => setTimeout(() => { el.style.transition = 'opacity 0.4s ease, filter 0.4s ease'; el.style.opacity = ''; el.style.filter = ''; }, i * 50));
    }

    function clearInline(el) {
        for (const p of ['height', 'overflow', 'paddingTop', 'paddingBottom', 'marginTop', 'borderWidth', 'maxWidth']) el.style[p] = '';
    }

    function slide(open) {
        const el = box;
        if (!el) return;
        if (raf) { cancelAnimationFrame(raf); raf = null; }
        const cs = getComputedStyle(el);
        const pad = parseFloat(cs.paddingTop), mt = parseFloat(cs.marginTop), bw = parseFloat(cs.borderTopWidth);
        if (open) {
            const parentW = el.parentElement.getBoundingClientRect().width;
            hidden = false;
            el.classList.remove('hidden');
            el.style.height = 'auto';
            const natural = el.getBoundingClientRect().height;
            const naturalW = el.parentElement.getBoundingClientRect().width;
            const widthGrows = naturalW > parentW;
            el.style.overflow = 'hidden';
            el.style.height = '0px'; el.style.paddingTop = '0px'; el.style.paddingBottom = '0px'; el.style.marginTop = '0px'; el.style.borderWidth = '0px';
            if (widthGrows) el.style.maxWidth = '0px';
            void el.offsetHeight;
            revealSections();
            const start = performance.now(), dur = 350;
            let lastTotal = 0;
            const frame = (now) => {
                const e = ease(Math.min((now - start) / dur, 1));
                const hgt = e * natural;
                el.style.height = hgt + 'px'; el.style.paddingTop = (e * pad) + 'px'; el.style.paddingBottom = (e * pad) + 'px';
                el.style.marginTop = (e * mt) + 'px'; el.style.borderWidth = (e * bw) + 'px';
                if (widthGrows) el.style.maxWidth = (e * naturalW) + 'px';
                const total = hgt + e * mt;
                h.scrollBy(total - lastTotal);
                lastTotal = total;
                if (e < 1) raf = requestAnimationFrame(frame); else { clearInline(el); raf = null; }
            };
            raf = requestAnimationFrame(frame);
        } else {
            const current = el.getBoundingClientRect().height, currentW = el.getBoundingClientRect().width;
            el.style.overflow = 'hidden';
            const start = performance.now(), dur = 250;
            let lastTotal = current + mt;
            const frame = (now) => {
                const x = Math.min((now - start) / dur, 1);
                const inv = 1 - ease(x);
                const hgt = inv * current;
                el.style.height = hgt + 'px'; el.style.paddingTop = (inv * pad) + 'px'; el.style.paddingBottom = (inv * pad) + 'px';
                el.style.marginTop = (inv * mt) + 'px'; el.style.borderWidth = (inv * bw) + 'px'; el.style.maxWidth = (inv * currentW) + 'px';
                const total = hgt + inv * mt;
                h.scrollBy(total - lastTotal);
                lastTotal = total;
                if (x < 1) raf = requestAnimationFrame(frame); else { hidden = true; clearInline(el); raf = null; }
            };
            raf = requestAnimationFrame(frame);
        }
    }

    // The first render lands in the stored state without a slide; a toggle slides.
    let mounted = false;
    $effect(() => {
        const open = t.open;
        if (!box) return;
        if (!mounted) { mounted = true; hidden = !open; if (open && t.phase === 'ready') revealSections(); return; }
        slide(open);
    });
    $effect(() => () => { if (raf) cancelAnimationFrame(raf); });

    function langInto(node, text) { node.textContent = text; h.twemojify(node); return { update(next) { node.textContent = next; h.twemojify(node); } }; }
    const langLine = $derived(h.autoTranslate() && t.lang && t.lang !== 'auto' && t.lang !== 'GB' ? `Original language: ${t.lang} ${h.flag(t.lang)}` : '');
</script>

<div class="transcription-result" class:hidden bind:this={box}>
    {#if t.phase === 'error'}
        <div class="transcription-error">Error: {t.error}</div>
    {:else}
        <div class="transcription-text">
            {#each t.sections as s, i (i)}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span class="transcription-section" class:current={currentIdx === i} data-timestamp={s.at} style="cursor: pointer;" use:section={i}
                      onclick={() => onSeek(parseFloat(s.at))}>{s.text}</span>{#if i < t.sections.length - 1}{' '}{/if}
            {:else}
                <span>No transcription available</span>
            {/each}
            {#if langLine}
                <div style="font-size: 0.8em; color: rgba(255, 255, 255, 0.6); margin-top: 5px;" use:langInto={langLine}></div>
            {/if}
        </div>
    {/if}
</div>
