<script>
    // A voice message's transcription: clickable sections that seek, the one under the
    // playhead lit, the detected language named. `open` slides the box in and out (height,
    // padding, margin and border together, the conversation held at its distance from the
    // bottom); the sections blur in with a stagger on every open.
    import { untrack } from 'svelte';

    let { t, positionMs, playing, h, onSeek, onSettled = () => {} } = $props();   // h: AudioPlayerHelpers (js/voice.js)
    // t: { phase: 'ready' | 'error', sections: [{ at, text }], lang, language, error, open, fresh };
    // h: flag(lang), twemojify(el), holdScroll(). onSettled: a fresh transcript has slid in.

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
            const hold = h.holdScroll();
            const frame = (now) => {
                const e = ease(Math.min((now - start) / dur, 1));
                const hgt = e * natural;
                el.style.height = hgt + 'px'; el.style.paddingTop = (e * pad) + 'px'; el.style.paddingBottom = (e * pad) + 'px';
                el.style.marginTop = (e * mt) + 'px'; el.style.borderWidth = (e * bw) + 'px';
                if (widthGrows) el.style.maxWidth = (e * naturalW) + 'px';
                hold();
                if (e < 1) raf = requestAnimationFrame(frame); else { clearInline(el); hold(); raf = null; }
            };
            raf = requestAnimationFrame(frame);
        } else {
            const current = el.getBoundingClientRect().height, currentW = el.getBoundingClientRect().width;
            el.style.overflow = 'hidden';
            const start = performance.now(), dur = 250;
            const hold = h.holdScroll();
            const frame = (now) => {
                const x = Math.min((now - start) / dur, 1);
                const inv = 1 - ease(x);
                const hgt = inv * current;
                el.style.height = hgt + 'px'; el.style.paddingTop = (inv * pad) + 'px'; el.style.paddingBottom = (inv * pad) + 'px';
                el.style.marginTop = (inv * mt) + 'px'; el.style.borderWidth = (inv * bw) + 'px'; el.style.maxWidth = (inv * currentW) + 'px';
                hold();
                if (x < 1) raf = requestAnimationFrame(frame); else { hidden = true; clearInline(el); hold(); raf = null; }
            };
            raf = requestAnimationFrame(frame);
        }
    }

    // The first render lands in the stored state without a slide; a toggle slides.
    let mounted = false;
    // What the box last slid to. The transcription is replaced whole on every patch, so
    // only a change of `open` may slide it; anything else would restart a slide mid-way.
    let shown = null;
    $effect(() => {
        const open = t.open;
        if (!box) return;
        if (!mounted) {
            mounted = true;
            shown = open;
            // A transcript that just arrived slides in like any open; one remounting with
            // its row (scrolled back into view) lands as it was.
            if (open && untrack(() => t.fresh)) { slide(true); onSettled(); return; }
            hidden = !open;
            if (open && t.phase === 'ready') revealSections();
            return;
        }
        if (open === shown) return;
        shown = open;
        slide(open);
    });
    $effect(() => () => { if (raf) cancelAnimationFrame(raf); });

    function langInto(node, text) { node.textContent = text; h.twemojify(node); return { update(next) { node.textContent = next; h.twemojify(node); } }; }
    // The detected language, named in English and in itself ("Russian (Русский)"). English
    // goes unsaid; a transcript from before the language was reported has no name to give.
    const language = $derived.by(() => {
        const code = t.language;
        if (!code || code === 'en' || !t.lang || t.lang === 'auto') return null;
        try {
            const english = new Intl.DisplayNames(['en'], { type: 'language' }).of(code);
            const own = new Intl.DisplayNames([code], { type: 'language' }).of(code);
            const native = own ? own.charAt(0).toLocaleUpperCase(code) + own.slice(1) : '';
            return { flag: h.flag(t.lang), name: native && native !== english ? `${english} (${native})` : english };
        } catch (_) {
            return null;
        }
    });
</script>

<div class="transcription-result" class:hidden bind:this={box}>
    {#if t.phase === 'error'}
        <div class="transcription-error">Error: {t.error}</div>
    {:else}
        {#if language}
            <div class="transcription-lang">
                <span>Language Detected:</span>
                <span class="transcription-lang-name"><span use:langInto={language.flag}></span>{language.name}</span>
            </div>
        {/if}
        <div class="transcription-text">
            {#each t.sections as s, i (i)}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span class="transcription-section" class:current={currentIdx === i} data-timestamp={s.at} style="cursor: pointer;" use:section={i}
                      onclick={() => onSeek(parseFloat(s.at))}>{s.text}</span>{#if i < t.sections.length - 1}{' '}{/if}
            {:else}
                <span>No transcription available</span>
            {/each}
        </div>
    {/if}
</div>
