<script>
    // The composer box: the scroll-return button, the reply and command strips, and the
    // input row around the editor. The editor (composer.js) is an imperative leaf built
    // into #chat-input-host by main.js; everything else derives from lib/composer.
    import { composerMode, composerDraft, composerLock, composerCommand, composerStatus, composerChrome, composerHandlers, composerEls } from '../lib/composer.svelte.js';
    import CommandStrip from './CommandStrip.svelte';
    import VoiceRecorderUI from './VoiceRecorderUI.svelte';
    import { recorderState } from '../lib/voicerecorder.svelte.js';

    const mode = composerMode();
    const draft = composerDraft();
    const lock = composerLock();
    const status = composerStatus();
    const command = composerCommand();
    const chrome = composerChrome();
    const els = composerEls();
    const h = () => composerHandlers();
    const voice = recorderState();
    // A recording or its preview takes the row over: the controls and the editor step
    // aside, the mic goes once the finger is off it, and Send returns for the preview.
    const voiceBusy = $derived(voice.state === 'recording' || voice.state === 'locked' || voice.state === 'preview');
    const voiceHidesMic = $derived(voice.state === 'locked' || voice.state === 'preview');
    const voicePreview = $derived(voice.state === 'preview');
    // Plays the return fade once per tick on a control that is back on screen.
    function fadeIn(node, tick) {
        return { update() {
            node.classList.add('chat-input-fade-in');
            node.addEventListener('animationend', () => node.classList.remove('chat-input-fade-in'), { once: true });
        } };
    }

    const isReply = $derived(mode.kind === 'reply');
    const isEdit = $derived(mode.kind === 'edit');
    const locked = $derived(!!lock.reason);
    // The send button shows for a non-empty draft and always while editing.
    const showSend = $derived(!draft.empty || isEdit);

    let replyCancel = $state(null);
    let cancelRight = $state('');

    // The reply bar's content stays through the collapse animation, so it only
    // re-renders while a reply is up.
    function nameInto(node, name) {
        const apply = (n) => { node.textContent = n; h()?.twemojify(node); };
        apply(name);
        return { update: apply };
    }
    function snippetInto(node, s) {
        const apply = (snip) => {
            node.textContent = '';
            if (snip && snip.html) {
                node.innerHTML = snip.html;
                h()?.twemojify(node);
                if (snip.emojiTags?.length) h()?.renderCustomEmojiShortcodes(node, snip.emojiTags);
            } else if (snip && snip.text) {
                node.textContent = snip.text;
            }
        };
        apply(s);
        return { update: apply };
    }
    // Centre the cancel icon over the mic/send column: those buttons flex-shrink with
    // the window, so the offset is measured, not assumed.
    $effect(() => {
        if (!isReply || !replyCancel) return;
        mode.snippet; mode.name;
        const slotBtn = els.voice?.offsetParent ? els.voice : els.send;
        const slotRect = slotBtn?.getBoundingClientRect();
        if (!slotRect || slotRect.width <= 0) return;
        const barRect = replyCancel.parentElement.getBoundingClientRect();
        // 11.6 = half the 24px button minus a 0.4px optical correction.
        cancelRight = `${(barRect.right - slotRect.left - slotRect.width / 2 - 11.6).toFixed(1)}px`;
    });

    // The editor is not ours to render, so its display, lock and placeholder are set
    // on it. The property, not the attribute: the rich composer draws its placeholder
    // from data-placeholder (its Proxy maps the property there); a textarea maps it
    // to the attribute. Either way the property is the one channel that renders.
    $effect(() => {
        const input = h()?.input();
        if (!input) return;
        input.style.display = command.active || voiceBusy ? 'none' : '';
    });
    $effect(() => {
        voice.fadeTick;
        const input = h()?.input();
        if (!input || !voice.fadeTick) return;
        input.classList.add('chat-input-fade-in');
        input.addEventListener('animationend', () => input.classList.remove('chat-input-fade-in'), { once: true });
    });
    $effect(() => {
        const input = h()?.input();
        if (!input) return;
        input.disabled = locked;
        input.style.paddingLeft = locked ? '15px' : '';
        input.placeholder = locked ? lock.placeholder : status.text ? status.text : (isEdit ? 'Editing message...' : h().placeholder);
    });

    // ── mic ↔ send ──
    // Typed changes animate the swap; programmatic ones (a chat open, a send, an edit)
    // snap, because a WebKit swap animation wedges if the composer hides mid-flight.
    let shown = null;   // 'send' | 'voice' | null, what is on screen
    let sendShown = $state(false);
    let voiceShown = $state(true);
    let sendActive = $state(false);
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
    // The timer picker opens from a right-click or a long press on Send.
    let pressTimer = null;
    function openTimer() {
        if (els.send) h()?.openSelfDestruct?.(els.send.getBoundingClientRect());
    }
    function pressStart() { pressTimer = setTimeout(openTimer, 500); }
    function pressEnd() { if (pressTimer) { clearTimeout(pressTimer); pressTimer = null; } }
    let swappingOut = $state(false);
    // Which swap class each button wears: 'out' is the one leaving, 'in' the one arriving.
    let sendAnim = $state(null);
    let voiceAnim = $state(null);
    let wanted = null;   // the swap in flight, so one superseded mid-animation is dropped

    function clearAnim() { sendAnim = null; voiceAnim = null; }
    function snap(want) {
        clearAnim();
        wanted = null;
        swappingOut = false;
        shown = want;
        sendActive = want === 'send';
        sendShown = want === 'send';
        voiceShown = want === 'voice';
    }
    function swap(want) {
        clearAnim();
        shown = want;
        sendActive = want === 'send';
        wanted = want;
        swappingOut = want === 'voice';
        if (want === 'send') voiceAnim = 'out'; else sendAnim = 'out';
    }
    /** The leaving button's animation ends the swap; the arriving one's just clears the class. */
    function swapAnimEnd(which) {
        if (which === 'send' && sendAnim === 'in') return void (sendAnim = null);
        if (which === 'voice' && voiceAnim === 'in') return void (voiceAnim = null);
        const want = wanted;
        if (which === 'voice' && voiceAnim === 'out') {
            voiceAnim = null;
            voiceShown = false;
        } else if (which === 'send' && sendAnim === 'out') {
            sendAnim = null;
            sendShown = false;
        } else {
            return;
        }
        swappingOut = false;
        wanted = null;
        if (shown !== want) return;   // superseded mid-flight
        if (want === 'send') { sendShown = true; sendAnim = 'in'; }
        else { voiceShown = true; voiceAnim = 'in'; }
    }
</script>

<div class="row input-box" id="chat-box" class:replying={isReply} class:commanding={command.active} bind:this={els.box}>
    <!-- Anchored to #chat-box's top edge so it rides up as the composer grows. -->
    <button id="chat-scroll-return" class="corner-float scroll-return-btn" bind:this={els.scrollReturn}>
        <span class="icon icon-chevron-down"></span>
        <span class="scroll-return-badge" class:visible={!!chrome.scrollBadge}>{chrome.scrollBadge}</span>
    </button>
    <div id="msg-bottom-fade" class="fadeout-bottom-msgs"></div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chat-reply-bar" onclick={(e) => { if (e.target.closest('#chat-reply-bar-cancel')) return; h()?.jumpToReply(); }}>
        <span id="chat-reply-bar-label">Replying to <span id="chat-reply-bar-name" use:nameInto={mode.name}></span></span>
        <span id="chat-reply-bar-snippet" use:snippetInto={mode.snippet}></span>
        <button id="chat-reply-bar-cancel" bind:this={replyCancel} style:right={cancelRight || null} onclick={() => h()?.cancelReply()}><span class="icon icon-cancel"></span></button>
    </div>
    <div id="chat-command-bar">
        <CommandStrip onCancel={() => h()?.cancelCommand()} />
    </div>
    <div class="row chat-input-container" bind:this={els.container}>
        <button id="chat-input-file" class:open={chrome.attachmentOpen} style:display={isEdit || locked || voiceBusy ? 'none' : null} bind:this={els.file} use:fadeIn={voice.fadeTick} onclick={() => h()?.toggleAttachments()}><span class="icon icon-plus"></span></button>
        <button id="chat-input-cancel" style:display={isEdit ? null : 'none'} onclick={() => h()?.cancel()}><span class="icon icon-cancel"></span></button>
        <VoiceRecorderUI part="strip" />
        <div id="chat-input-host" style:display={voiceBusy ? 'none' : null}></div>
        <button id="chat-input-emoji" style:display={locked || voiceBusy ? 'none' : null} bind:this={els.emoji} use:fadeIn={voice.fadeTick}><span class="icon {chrome.emojiIcon === 'wink' ? 'icon-wink-face' : 'icon-smile-face'}"></span></button>
        <button id="chat-input-voice" style="margin-right: 3px;" class:pending={voice.state === 'pending'} class:recording={voice.state === 'recording'} class:button-swap-in={voiceAnim === 'in'} class:button-swap-out={voiceAnim === 'out'} style:display={voiceShown && !voiceHidesMic ? null : 'none'} bind:this={els.voice} use:fadeIn={voice.fadeTick} onanimationend={() => swapAnimEnd('voice')} oncontextmenu={(e) => e.preventDefault()}><span class="icon icon-mic-on"></span></button>
        <VoiceRecorderUI part="dot" />
        <button id="chat-input-send" style="margin-right: 3px;" class:active={sendActive || voicePreview} class:voice-preview-send={voicePreview} class:has-self-destruct={!!chrome.selfDestructSecs} class:button-swap-in={sendAnim === 'in'} class:button-swap-out={sendAnim === 'out'} data-sd-secs={chrome.selfDestructSecs || undefined} style:display={sendShown || voicePreview ? null : 'none'} bind:this={els.send} onanimationend={() => swapAnimEnd('send')} onclick={() => h()?.send()}
                oncontextmenu={(e) => { e.preventDefault(); openTimer(); }} ontouchstart={pressStart} ontouchend={pressEnd} ontouchmove={pressEnd} ontouchcancel={pressEnd}><span class="icon icon-send"></span></button>
        <!-- Outside the send button so it never inherits the mic/send swap rotation; it fades with the button. -->
        <span class="self-destruct-badge" class:is-visible={!!chrome.selfDestructSecs && sendShown && !swappingOut}><svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/></svg></span>
    </div>
    <VoiceRecorderUI part="overlays" />
</div>
