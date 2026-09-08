<script>
    // The composer's chrome as ONE reconciler (Phase 3a). Renderless: the markup stays in
    // index.html and five modules keep their cached element references, so this adopts
    // those elements and derives their state from lib/composer.svelte.js instead of six
    // modules writing to them. The editor element is never touched.
    import { composerMode, composerDraft, composerLock, composerCommand, composerStatus } from '../lib/composer.svelte.js';

    let { els, h } = $props();   // els: box, input, file, cancel, emoji, voice, send, replyName, replySnippet, replyCancel

    const mode = composerMode();
    const draft = composerDraft();
    const lock = composerLock();
    const status = composerStatus();
    const command = composerCommand();

    const isReply = $derived(mode.kind === 'reply');
    const isEdit = $derived(mode.kind === 'edit');
    const locked = $derived(!!lock.reason);
    // The send button shows for a non-empty draft and always while editing.
    const showSend = $derived(!draft.empty || isEdit);

    // ── the reply bar ──
    $effect(() => {
        els.box.classList.toggle('replying', isReply);
    });
    $effect(() => {
        if (!isReply) return;   // content stays for the collapse animation
        const name = mode.name;
        els.replyName.textContent = name;
        h.twemojify(els.replyName);
        const s = mode.snippet;
        els.replySnippet.textContent = '';
        if (s && s.html) {
            els.replySnippet.innerHTML = s.html;
            h.twemojify(els.replySnippet);
            if (s.emojiTags?.length) h.renderCustomEmojiShortcodes(els.replySnippet, s.emojiTags);
        } else if (s && s.text) {
            els.replySnippet.textContent = s.text;
        }
        // Centre the cancel icon over the mic/send column: those buttons flex-shrink with
        // the window, so the offset is measured, not assumed.
        const slotBtn = els.voice.offsetParent ? els.voice : els.send;
        const slotRect = slotBtn.getBoundingClientRect();
        if (slotRect.width > 0) {
            const barRect = els.replyCancel.parentElement.getBoundingClientRect();
            // 11.6 = half the 24px button minus a 0.4px optical correction.
            els.replyCancel.style.right = `${(barRect.right - slotRect.left - slotRect.width / 2 - 11.6).toFixed(1)}px`;
        }
    });

    // ── the command composer replaces the editor while it is up ──
    // Its pills mount in the same flush this runs after, so the editor is never
    // taken away before its replacement is there.
    $effect(() => {
        els.box.classList.toggle('commanding', command.active);
        els.input.style.display = command.active ? 'none' : '';
    });

    // ── file ↔ cancel, placeholder, lock ──
    $effect(() => {
        els.file.style.display = isEdit || locked ? 'none' : '';
        els.cancel.style.display = isEdit ? '' : 'none';
    });
    $effect(() => {
        els.input.disabled = locked;
        els.input.style.paddingLeft = locked ? '15px' : '';
        els.emoji.style.display = locked ? 'none' : '';
        // The property, not the attribute: the rich composer draws its placeholder from
        // data-placeholder (its Proxy maps the property there); a textarea maps it to the
        // attribute. Either way the property is the one channel that renders.
        els.input.placeholder = locked ? lock.placeholder : status.text ? status.text : (isEdit ? 'Editing message...' : h.placeholder);
    });

    // ── mic ↔ send ──
    // Typed changes animate the swap; programmatic ones (a chat open, a send, an edit)
    // snap, because a WebKit swap animation wedges if the composer hides mid-flight.
    let shown = null;   // 'send' | 'voice' | null, what is on screen
    $effect(() => {
        draft.seq;
        const want = showSend ? 'send' : 'voice';
        const animate = draft.animate && !locked;
        if (locked) {
            snap('none');
            return;
        }
        if (want === shown) return;
        if (animate && shown !== null) swap(want);
        else snap(want);
    });
    function clearAnim() {
        els.send.classList.remove('button-swap-in', 'button-swap-out');
        els.voice.classList.remove('button-swap-in', 'button-swap-out');
    }
    function snap(want) {
        clearAnim();
        shown = want;
        els.send.classList.toggle('active', want === 'send');
        els.send.style.display = want === 'send' ? '' : 'none';
        els.voice.style.display = want === 'voice' ? '' : 'none';
    }
    function swap(want) {
        const out = want === 'send' ? els.voice : els.send;
        const inn = want === 'send' ? els.send : els.voice;
        clearAnim();
        shown = want;
        els.send.classList.toggle('active', want === 'send');
        out.classList.add('button-swap-out');
        out.addEventListener('animationend', () => {
            out.style.display = 'none';
            out.classList.remove('button-swap-out');
            if (shown !== want) return;   // superseded mid-flight
            inn.style.display = '';
            inn.classList.add('button-swap-in');
            inn.addEventListener('animationend', () => inn.classList.remove('button-swap-in'), { once: true });
        }, { once: true });
    }
</script>
