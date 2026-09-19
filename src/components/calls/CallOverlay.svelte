<script>
    // The call on screen: a ring card while a call comes in, otherwise a pill that
    // floats above every pane, drags anywhere, remembers where it was left, and opens
    // into volume, microphone and a plain-language view of the connection. Video is
    // a section of the same pill, between its header and that panel: their pictures
    // with ours in a corner. Any of theirs can fill the window.
    import { untrack } from 'svelte';
    import { slide } from 'svelte/transition';
    import { callState, callAudio, callVideo, setMaximized } from '../lib/calls.svelte.js';
    import { showTooltip, hideTooltip } from '../lib/tooltip.svelte.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';
    import VoiceMeter from './VoiceMeter.svelte';

    /**
     * @typedef {object} CallHelpers
     * @property {() => void} accept
     * @property {() => void} reject
     * @property {() => void} hangup
     * @property {(on: boolean) => void} setMuted
     * @property {(volume: number) => void} setVolume
     * @property {(kind: 'camera'|'screen', on: boolean) => void} setVideo
     * @property {() => void} changeScreen
     * @property {(kind: 'camera'|'screen', rung: number|null, fps: number|null) => void} setVideoPrefs
     * @property {(on: boolean) => void} setShareAudio
     * @property {(volume: number) => void} setShareVolume
     * @property {(kind: 'camera'|'screen', el: HTMLCanvasElement, gone?: boolean) => void} peerCanvas
     * @property {(kind: 'camera'|'screen', el: HTMLVideoElement|null) => void} selfPreview
     * @property {(patch: {autoGain?: boolean, echoCancel?: boolean, noiseSuppress?: boolean}) => void} setAudio
     * @property {(npub: string) => object|null} getProfile
     * @property {(profileOrId: object|string) => string} getName
     * @property {(profile: object|null) => string|null} getProfileAvatarSrc
     * @property {(npub: string) => void} openChat
     */
    /** @type {{ h: CallHelpers }} */
    let { h } = $props();
    const c = callState();
    const a = callAudio();
    const v = callVideo();

    const REASONS = {
        hangup: 'Call ended', rejected: 'Declined', busy: 'Busy', no_answer: 'No answer',
        missed: 'Missed call', connect_failed: 'Could not connect', disconnected: 'Connection lost',
        audio_failed: 'Microphone unavailable', send_failed: 'Could not reach the relays',
        account_changed: 'Call ended', answered_elsewhere: 'Answered on another device',
    };
    const QUALITY = { excellent: 'Excellent', good: 'Good', fair: 'Fair', poor: 'Poor' };

    const peer = $derived.by(() => {
        c.tick;
        if (!c.peer) return null;
        profileVersion(c.peer);
        const p = h.getProfile(c.peer) || null;
        return { name: h.getName(p || c.peer), avatar: h.getProfileAvatarSrc(p) || null };
    });

    // The timer runs on a local clock between state events.
    let now = $state(Date.now());
    $effect(() => {
        if (c.phase !== 'active') return;
        now = Date.now();
        const t = setInterval(() => { now = Date.now(); }, 1000);
        return () => clearInterval(t);
    });
    function clock(ms) {
        const s = Math.floor(ms / 1000);
        const m = Math.floor(s / 60);
        const hh = Math.floor(m / 60);
        const mm = String(m % 60).padStart(2, '0');
        const ss = String(s % 60).padStart(2, '0');
        return hh ? `${hh}:${mm}:${ss}` : `${mm}:${ss}`;
    }
    const status = $derived.by(() => {
        switch (c.phase) {
            case 'ringing': return c.outgoing ? 'Calling…' : 'Incoming call';
            case 'connecting': return 'Connecting…';
            case 'active': return clock(c.activeMs + (now - c.receivedAt));
            case 'ended': return REASONS[c.reason] || 'Call ended';
            default: return '';
        }
    });
    const live = $derived(c.phase === 'active');
    // This device can send video at all, and the peer can take it.
    const canSend = $derived(v.encode.length > 0 && c.peerDecodes.length > 0);
    const mineOn = $derived(c.videoMine.camera || c.videoMine.screen);
    const peerOn = $derived(c.videoPeer.camera || c.videoPeer.screen);
    const videoOn = $derived(live && (mineOn || peerOn));
    const pictures = $derived(videoOn);
    // Of their pictures, the screen leads and the camera sits inset; alone, either leads.
    const lead = $derived(c.videoPeer.screen ? 'screen' : c.videoPeer.camera ? 'camera' : null);
    const inset = $derived(c.videoPeer.screen && c.videoPeer.camera ? 'camera' : null);
    const aspectOf = (kind) => { const p = kind && v.peer[kind]; return p && p.width ? `${p.width} / ${p.height}` : '16 / 9'; };
    // The window-filling view of one of theirs; the other, if any, rides along the bottom.
    const max = $derived(live && v.maximized && c.videoPeer[v.maximized] ? v.maximized : null);
    const maxOther = $derived(max ? (max === 'screen' ? (c.videoPeer.camera ? 'camera' : null) : (c.videoPeer.screen ? 'screen' : null)) : null);
    const maxQuality = $derived(max && c.quality && c.quality !== 'excellent' && c.quality !== 'good' ? c.quality : null);
    let maxControls = $state(false);
    let maxHide = null;
    function wakeControls() {
        maxControls = true;
        clearTimeout(maxHide);
        maxHide = setTimeout(() => { maxControls = false; }, 2500);
    }
    $effect(() => {
        if (!max) return;
        const key = (e) => { if (e.key === 'Escape') setMaximized(null); };
        window.addEventListener('keydown', key);
        return () => window.removeEventListener('keydown', key);
    });
    // The media surfaces are the only elements this component hands out.
    function peerCanvas(node, kind) {
        h.peerCanvas(kind, node);
        return { destroy() { h.peerCanvas(kind, node, true); } };
    }
    function selfPreview(node, kind) {
        h.selfPreview(kind, node);
        return { destroy() { h.selfPreview(kind, null); } };
    }
    // A vertical curtain: the height alone, nothing inside rescaled. A block nested in
    // the panel has no layout yet when a transition is created, so the stock slide
    // reads it as empty; this one measures on its first frame instead.
    function curtain(node, { duration = 180 } = {}) {
        // The first tick comes before the block is in the document, where it measures
        // as nothing; a zero is never kept, the next frame measures again.
        let h = 0;
        const ease = (t) => 1 - Math.pow(1 - t, 3);
        return {
            duration,
            tick: (t) => {
                if (!h) {
                    node.style.height = '';
                    h = node.getBoundingClientRect().height;
                    if (!h) return;
                }
                const open = t >= 1;
                node.style.overflow = open ? '' : 'hidden';
                node.style.height = open ? '' : `${(ease(t) * h).toFixed(1)}px`;
                node.style.minHeight = open ? '' : '0';
            },
        };
    }
    // Vector's own tooltip on a thumbnail, only while there is something to say.
    function unseenTip(e, kind) {
        if (!c.pausedByPeer) return;
        showTooltip(`${peer?.name || 'They'} is busy and can't see your ${kind}`, e.currentTarget.getBoundingClientRect());
    }
    // The panel's quality choices, by rung on each ladder.
    const QUALITY_CAMERA = [['Low', 1], ['Medium', 3], ['High', 5], ['Best', 6]];
    const QUALITY_SCREEN = [['Low', 1], ['Medium', 3], ['High', 5], ['Best', 7], ['Ultra', 8]];
    const FPS_CAMERA = [15, 30];
    const FPS_SCREEN = [5, 10, 15, 30, 60];
    function choosePref(kind, field, value) {
        const p = v.prefs[kind];
        const next = { rung: p.rung, fps: p.fps, [field]: value === '' ? null : Number(value) };
        h.setVideoPrefs(kind, next.rung, next.fps);
    }
    const trackLine = (kind) => {
        const t = c.stats?.video?.[kind];
        return t && c.videoMine[kind] && t.width ? `${t.width}×${t.height} · ${t.fps} fps · ${t.kbps} kbit/s` : '';
    };
    const peerTrackLine = (kind) => {
        const p = v.peer[kind];
        return c.videoPeer[kind] && p.width ? `${p.width}×${p.height} · ${v.decFps[kind]} fps` : '';
    };

    // ── the pill's place: dragged anywhere, remembered per device ──
    const POS_KEY = 'call_pill_pos';
    let pos = $state(loadPos());
    let expanded = $state(false);
    let pill = $state(null);
    let drag = null;
    function loadPos() {
        try {
            const p = JSON.parse(localStorage.getItem(POS_KEY));
            if (p && Number.isFinite(p.x) && Number.isFinite(p.y)) return p;
        } catch (_) {}
        return null;
    }
    function savePos() {
        try { localStorage.setItem(POS_KEY, JSON.stringify(pos)); } catch (_) {}
    }
    // The window chrome's strip is off limits: the pill must never cover its buttons.
    function chromeHeight() {
        const v = parseFloat(getComputedStyle(document.body).getPropertyValue('--chrome-h'));
        return Number.isFinite(v) ? v : 0;
    }
    function clamp(p) {
        const w = pill?.offsetWidth || 300;
        const hgt = pill?.offsetHeight || 48;
        const top = chromeHeight() + 8;
        const x = Math.min(Math.max(8, p.x), Math.max(8, window.innerWidth - w - 8));
        const y = Math.min(Math.max(top, p.y), Math.max(top, window.innerHeight - hgt - 8));
        // The same object when nothing moved, so a re-clamp never counts as a change.
        return x === p.x && y === p.y ? p : { x, y };
    }
    function onPointerDown(e) {
        if (e.button !== 0 || e.target.closest('button, input, a')) return;
        const r = pill.getBoundingClientRect();
        drag = { sx: e.clientX, sy: e.clientY, ox: r.left, oy: r.top, moved: false };
        // Capture on the row that listens, so its own pointerup ends the drag.
        e.currentTarget.setPointerCapture(e.pointerId);
    }
    function onPointerMove(e) {
        if (!drag) return;
        const dx = e.clientX - drag.sx;
        const dy = e.clientY - drag.sy;
        // A few pixels of slop separate a tap from a drag.
        if (!drag.moved && Math.hypot(dx, dy) < 4) return;
        drag.moved = true;
        pos = clamp({ x: drag.ox + dx, y: drag.oy + dy });
    }
    function onPointerUp() {
        if (!drag) return;
        const moved = drag.moved;
        drag = null;
        if (moved) savePos();
        else toggle();
    }

    // Width is animated by hand: the closed pill hugs its content, which CSS cannot
    // tween from, so the width is pinned for the transition and released after.
    const WIDTH_MS = 180;
    // The open width is pinned inline for the animation, so it must respect the
    // window itself; the stylesheet's cap cannot reach an inline value.
    let winW = $state(window.innerWidth);
    const openW = $derived(Math.min(pictures ? 480 : 300, winW - 24));
    let width = $state(null);
    let collapsedW = 0;
    let release = null;
    // Measured only while unpinned, so a mid-flight width never poses as the resting one.
    $effect(() => {
        if (!expanded && width == null && pill) collapsedW = pill.offsetWidth;
    });
    function toggle() {
        if (!pill) { expanded = !expanded; return; }
        clearTimeout(release);
        const from = pill.offsetWidth;
        if (!expanded && width == null) collapsedW = from;
        width = from;
        expanded = !expanded;
        const opening = expanded;
        requestAnimationFrame(() => requestAnimationFrame(() => {
            if (opening !== expanded) return;
            width = opening ? openW : (collapsedW || from);
            // Released on a clock, not the transition's end event: a close that lands
            // on its current width never transitions, and the meters' end events bubble.
            if (!opening) release = setTimeout(() => { if (!expanded) width = null; }, WIDTH_MS + 40);
        }));
    }
    $effect(() => {
        const keep = () => {
            winW = window.innerWidth;
            if (width != null && expanded) width = Math.min(width, winW - 24);
            if (pos) pos = clamp(pos);
        };
        window.addEventListener('resize', keep);
        return () => window.removeEventListener('resize', keep);
    });
    // Opening the panel can push the pill off the bottom; pull it back in. The
    // position is read untracked: writing it from an effect that depends on it loops.
    $effect(() => {
        expanded;
        pictures;
        queueMicrotask(() => untrack(() => { if (pos) pos = clamp(pos); }));
    });
    $effect(() => {
        pictures;
        untrack(() => { if (width != null) width = expanded ? openW : null; });
    });
    const placement = $derived((pos ? `left:${pos.x}px; top:${pos.y}px; transform:none;` : '') + (width != null ? ` width:${width}px;` : ''));

    // How brightly the peer's avatar ring glows, 0 to 1. Levels sit on a -60 dB scale,
    // so the floor is lifted above room noise and speech spans the rest.
    const talk = $derived(live && !c.peerMuted ? Math.min(1, Math.max(0, (c.levels.peer - 0.3) / 0.55)) : 0);

    // ── the connection in words and a picture ──
    const delayMs = $derived(c.stats?.rtt_ms ?? null);
    const path = $derived(c.stats?.path === 'relay' ? 'via relay' : c.stats?.path === 'direct' ? 'direct' : '');
    const lostPct = $derived.by(() => {
        const recent = c.history.slice(-10);
        if (!recent.length) return 0;
        return recent.reduce((a, r) => a + r.lost, 0) / recent.length;
    });
    const lostText = $derived(lostPct < 0.05 ? 'none' : lostPct < 1 ? 'under 1%' : `${lostPct.toFixed(lostPct < 10 ? 1 : 0)}%`);
    // The sending bitrate, which follows the connection: the top of the ladder on a
    // clean link, lower while the network is dropping our packets.
    const bitrate = $derived(c.stats?.bitrate_kbps || null);
    const bitrateHint = $derived.by(() => {
        const loss = c.stats?.net_loss ?? 0;
        if (loss >= 4) return 'lowered, the network is dropping packets';
        if (bitrate && bitrate >= 64) return 'full quality';
        return '';
    });
    // Graph: delay as a line, lost audio as bars, over the last minute. The top
    // HEAD pixels are headroom for the tip, so it can sit above the highest sample.
    const W = 240, H = 72, HEAD = 34;
    const graph = $derived.by(() => {
        const hist = c.history;
        if (hist.length < 2) return null;
        const n = 60;
        const step = W / (n - 1);
        const off = n - hist.length;
        const maxRtt = Math.max(200, ...hist.map(r => r.rtt));
        const pts = hist.map((r, i) => ({ x: (off + i) * step, y: H - 2 - (r.rtt / maxRtt) * (H - HEAD - 6), rtt: r.rtt, lost: r.lost, ago: hist.length - 1 - i }));
        const line = pts.map(p => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(' ');
        const bars = pts.map(p => ({ x: p.x, hgt: Math.min(H - HEAD - 4, (p.lost / 10) * (H - HEAD - 4)) })).filter(b => b.hgt > 0.5);
        return { line, bars, maxRtt, step, pts };
    });
    // The sample under the pointer, as the Blossom chart does it: nearest by x.
    let hover = $state(null);
    function pick(e) {
        if (!graph) return;
        const r = e.currentTarget.getBoundingClientRect();
        const x = ((e.clientX - r.left) / r.width) * W;
        let best = graph.pts[0];
        for (const p of graph.pts) if (Math.abs(p.x - x) < Math.abs(best.x - x)) best = p;
        hover = best;
    }
    function leave() { hover = null; }
    // The tip sits above its sample; near either side it hangs inward from that side.
    const tipSide = $derived(!hover ? '' : hover.x < W * 0.25 ? 'call-graph-tip-left' : hover.x > W * 0.75 ? 'call-graph-tip-right' : '');
    function lostWord(v) { return v < 0.05 ? 'nothing lost' : v < 1 ? 'under 1% lost' : `${v.toFixed(v < 10 ? 1 : 0)}% lost`; }
</script>

{#if c.id && c.phase === 'ringing' && !c.outgoing}
    <div class="call-ring-overlay">
        <div class="call-ring-card">
            <Avatar src={peer?.avatar} size={84} />
            <div class="call-ring-name">{peer?.name || ''}</div>
            <div class="call-ring-sub">{c.videoOffered ? 'Incoming video call' : 'Incoming call'}</div>
            <div class="call-ring-actions">
                <button class="call-round call-round-decline" title="Decline" onclick={() => h.reject()}>
                    <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
                <button class="call-round call-round-accept" title="Accept" onclick={() => h.accept()}>
                    <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            </div>
        </div>
    </div>
{:else if c.id}
    {#if max}
        <!-- One of their pictures over everything. The pill waits underneath. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="call-max" class:call-max-controls={maxControls} onpointermove={wakeControls} ondblclick={() => setMaximized(null)}>
            {#key max}
                <canvas class="call-max-canvas" use:peerCanvas={max}></canvas>
            {/key}
            {#if maxQuality}
                <span class="call-max-quality"><span class="call-dot call-dot-{maxQuality}"></span> {QUALITY[maxQuality]}</span>
            {/if}
            <button class="call-max-restore" title="Back to the call window (Esc)" onclick={() => setMaximized(null)}>
                <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M2.99988 8H3.19988C4.88004 8 5.72011 8 6.36185 7.67302C6.92634 7.3854 7.38528 6.92646 7.6729 6.36197C7.99988 5.72024 7.99988 4.88016 7.99988 3.2V3M2.99988 16H3.19988C4.88004 16 5.72011 16 6.36185 16.327C6.92634 16.6146 7.38528 17.0735 7.6729 17.638C7.99988 18.2798 7.99988 19.1198 7.99988 20.8V21M15.9999 3V3.2C15.9999 4.88016 15.9999 5.72024 16.3269 6.36197C16.6145 6.92646 17.0734 7.3854 17.6379 7.67302C18.2796 8 19.1197 8 20.7999 8H20.9999M15.9999 21V20.8C15.9999 19.1198 15.9999 18.2798 16.3269 17.638C16.6145 17.0735 17.0734 16.6146 17.6379 16.327C18.2796 16 19.1197 16 20.7999 16H20.9999" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            </button>
            {#if maxOther}
                <!-- Their other picture rides along the bottom; a click swaps the two. -->
                <button class="call-max-other" title="Swap" onclick={() => setMaximized(maxOther)}>
                    {#key maxOther}
                        <canvas use:peerCanvas={maxOther}></canvas>
                    {/key}
                </button>
            {/if}
            <div class="call-max-strip">
                <button class="call-btn" class:call-btn-on={c.videoMine.camera} title={c.videoMine.camera ? 'Turn the camera off' : 'Turn the camera on'} disabled={!canSend} onclick={() => h.setVideo('camera', !c.videoMine.camera)}>
                    <span class="icon icon-video"></span>
                </button>
                <button class="call-btn" class:call-btn-on={c.videoMine.screen} title={c.videoMine.screen ? 'Stop sharing the screen' : 'Share the screen'} disabled={!canSend} onclick={() => h.setVideo('screen', !c.videoMine.screen)}>
                    <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" stroke="currentColor" stroke-width="2"/><path d="M8 20h8M12 16v4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
                </button>
                <button class="call-btn" class:call-btn-on={c.muted} title={c.muted ? 'Unmute microphone' : 'Mute microphone'} onclick={() => h.setMuted(!c.muted)}>
                    <span class="icon" class:icon-mic-off={c.muted} class:icon-mic-on={!c.muted}></span>
                </button>
                <button class="call-btn call-btn-hangup" title="Hang up" onclick={() => h.hangup()}>
                    <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            </div>
        </div>
    {/if}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="call-pill" class:call-pill-open={expanded} class:call-pill-ended={c.phase === 'ended'} class:call-pill-live={live} class:call-pill-video={pictures} class:call-pill-under={!!max}
         style={placement} bind:this={pill}>
        <!-- Only the header row drags or toggles; what is below is for its controls. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="call-pill-row" onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp} onpointercancel={onPointerUp}>
            <span class="call-pill-avatar" style="--talk: {talk.toFixed(2)}">
                <Avatar src={peer?.avatar} size={30} />
            </span>
            <div class="call-pill-text">
                <span class="call-pill-name cutoff">{peer?.name || ''}</span>
                <span class="call-pill-status cutoff">
                    {#if live && c.quality}<span class="call-dot call-dot-{c.quality}" title={QUALITY[c.quality]}></span>{/if}
                    {status}{#if live && c.peerMuted} · muted{/if}
                </span>
            </div>
            {#if c.phase !== 'ended'}
                {#if live && canSend}
                    <button class="call-btn" class:call-btn-on={c.videoMine.camera} title={c.videoMine.camera ? 'Turn the camera off' : 'Turn the camera on'} onclick={() => h.setVideo('camera', !c.videoMine.camera)}>
                        <span class="icon icon-video"></span>
                    </button>
                    <button class="call-btn" class:call-btn-on={c.videoMine.screen} title={c.videoMine.screen ? 'Stop sharing the screen' : 'Share the screen'} onclick={() => h.setVideo('screen', !c.videoMine.screen)}>
                        <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" stroke="currentColor" stroke-width="2"/><path d="M8 20h8M12 16v4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
                    </button>
                {/if}
                <button class="call-btn" class:call-btn-on={c.muted} title={c.muted ? 'Unmute microphone' : 'Mute microphone'} onclick={() => h.setMuted(!c.muted)}>
                    <span class="icon" class:icon-mic-off={c.muted} class:icon-mic-on={!c.muted}></span>
                </button>
                <button class="call-btn call-btn-hangup" title="Hang up" onclick={() => h.hangup()}>
                    <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            {/if}
        </div>
        {#if pictures && !max}
            <div class="call-pictures" style="aspect-ratio: {aspectOf(lead)}" transition:curtain>
                {#if lead}
                    {#key lead}
                        <canvas class="call-picture-lead" use:peerCanvas={lead}></canvas>
                    {/key}
                    {#if !v.peer[lead].width}
                        <div class="call-pictures-idle"><span>Waiting for their {lead}…</span></div>
                    {/if}
                    <button class="call-btn call-btn-small call-picture-max" title="Fill the window" onclick={() => setMaximized(lead)}>
                        <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M8 3H7.8C6.11984 3 5.27976 3 4.63803 3.32698C4.07354 3.6146 3.6146 4.07354 3.32698 4.63803C3 5.27976 3 6.11984 3 7.8V8M8 21H7.8C6.11984 21 5.27976 21 4.63803 20.673C4.07354 20.3854 3.6146 19.9265 3.32698 19.362C3 18.7202 3 17.8802 3 16.2V16M21 8V7.8C21 6.11984 21 5.27976 20.673 4.63803C20.3854 4.07354 19.9265 3.6146 19.362 3.32698C18.7202 3 17.8802 3 16.2 3H16M21 16V16.2C21 17.8802 21 18.7202 20.673 19.362C20.3854 19.9265 19.9265 20.3854 19.362 20.673C18.7202 21 17.8802 21 16.2 21H16" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                    </button>
                    {#if lead === 'screen'}<span class="call-picture-tag">{peer?.name || 'They'} is sharing a screen</span>{/if}
                {:else}
                    <div class="call-pictures-idle">
                        <Avatar src={peer?.avatar} size={64} />
                        <span>Their camera is off</span>
                    </div>
                {/if}
                {#if inset}
                    <button class="call-picture-inset" title="Fill the window with their camera" onclick={() => setMaximized(inset)}>
                        {#key inset}
                            <canvas use:peerCanvas={inset}></canvas>
                        {/key}
                    </button>
                {/if}
                <!-- Our own pictures; dimmed with an eye-off badge while the peer is not looking. -->
                <div class="call-pictures-mine" class:call-pictures-mine-unseen={c.pausedByPeer}>
                    {#if c.videoMine.screen}
                        <!-- svelte-ignore a11y_no_static_element_interactions -->
                        <span class="call-picture-self-wrap call-picture-self-screen" onmouseenter={(e) => unseenTip(e, 'screen')} onmouseleave={hideTooltip}>
                            <!-- svelte-ignore a11y_media_has_caption -->
                            <video class="call-picture-self" use:selfPreview={'screen'} muted playsinline autoplay></video>
                            <span class="call-picture-unseen icon icon-eye-off"></span>
                        </span>
                    {/if}
                    {#if c.videoMine.camera}
                        <!-- svelte-ignore a11y_no_static_element_interactions -->
                        <span class="call-picture-self-wrap" onmouseenter={(e) => unseenTip(e, 'camera')} onmouseleave={hideTooltip}>
                            <!-- svelte-ignore a11y_media_has_caption -->
                            <video class="call-picture-self" use:selfPreview={'camera'} muted playsinline autoplay></video>
                            <span class="call-picture-unseen icon icon-eye-off"></span>
                        </span>
                    {/if}
                </div>
            </div>
        {/if}
        {#if expanded && c.phase !== 'ended'}
            <div class="call-panel" transition:slide={{ duration: 180 }}>
                <label class="call-panel-slider">
                    <span class="call-panel-label">Their volume</span>
                    <input type="range" min="0" max="200" step="5" value={Math.round(c.volume * 100)}
                           style="--slider-pct: {Math.round(c.volume * 50)}%"
                           oninput={(e) => h.setVolume(e.currentTarget.value / 100)} />
                    <span class="call-panel-value">{Math.round(c.volume * 100)}%</span>
                </label>
                {#if c.shareAudioPeer}
                    <label class="call-panel-slider" transition:curtain>
                        <span class="call-panel-label">Their screen</span>
                        <input type="range" min="0" max="200" step="5" value={Math.round(c.shareVolume * 100)}
                               style="--slider-pct: {Math.round(c.shareVolume * 50)}%"
                               oninput={(e) => h.setShareVolume(e.currentTarget.value / 100)} />
                        <span class="call-panel-value">{Math.round(c.shareVolume * 100)}%</span>
                    </label>
                {/if}
                {#if !c.muted}
                    <div class="call-panel-meters" transition:curtain>
                        <span class="call-panel-label">Your voice</span>
                        <VoiceMeter level={c.levels.mic} active={live} />
                    </div>
                {/if}
                {#each [['camera', 'Camera quality', QUALITY_CAMERA, FPS_CAMERA], ['screen', 'Screenshare quality', QUALITY_SCREEN, FPS_SCREEN]] as [kind, title, qualities, rates]}
                    {#if live && c.videoMine[kind]}
                      <!-- A picture's section draws down like a curtain as it turns on, and up as it goes. -->
                      <div class="call-panel-block" transition:curtain>
                        <div class="call-panel-section">
                            <span>{title}</span>
                            <span class="call-panel-section-note">{trackLine(kind)}</span>
                        </div>
                        <div class="call-panel-grid">
                            <span class="call-panel-label">Quality</span>
                            <select class="call-panel-select" value={v.prefs[kind].rung ?? ''} onchange={(e) => choosePref(kind, 'rung', e.currentTarget.value)}>
                                <option value="">Adaptive</option>
                                {#each qualities as [label, rung]}<option value={rung}>{label}</option>{/each}
                            </select>
                            <span class="call-panel-label">Frame rate</span>
                            <select class="call-panel-select" value={v.prefs[kind].fps ?? ''} onchange={(e) => choosePref(kind, 'fps', e.currentTarget.value)}>
                                <option value="">Adaptive</option>
                                {#each rates as f}<option value={f}>{f} fps</option>{/each}
                            </select>
                            {#if kind === 'screen'}
                                <span class="call-panel-label">Window</span>
                                <button class="call-panel-button" onclick={() => h.changeScreen()}>Change…</button>
                                <span class="call-panel-label">Sound</span>
                                {#if v.shareAudio.available}
                                    <label class="toggle-container call-switch call-switch-inline">
                                        <span class="call-switch-text">
                                            {c.shareAudioMine ? 'Sharing' : 'Not shared'}
                                            {#if c.shareAudioDenied && !c.shareAudioMine}
                                                <!-- svelte-ignore a11y_no_static_element_interactions -->
                                                <span class="call-warn" onmouseenter={(e) => showTooltip('Allow Vector under Screen & System Audio Recording in System Settings, then try again', e.currentTarget.getBoundingClientRect())} onmouseleave={hideTooltip}>System permission required</span>
                                            {/if}
                                        </span>
                                        <input type="checkbox" checked={c.shareAudioMine} onchange={(e) => h.setShareAudio(e.currentTarget.checked)}>
                                        <span class="neon-toggle"></span>
                                    </label>
                                {:else}
                                    <span class="call-panel-value call-panel-value-quiet">Not offered by this window</span>
                                {/if}
                            {/if}
                        </div>
                      </div>
                    {/if}
                {/each}
                <div class="call-panel-section">Voice processing</div>
                <div class="call-panel-switches">
                    <label class="toggle-container call-switch">
                        <span>Automatic gain</span>
                        <input type="checkbox" checked={a.autoGain} onchange={(e) => h.setAudio({ autoGain: e.currentTarget.checked })}>
                        <span class="neon-toggle"></span>
                    </label>
                    <label class="toggle-container call-switch">
                        <span>Echo cancellation</span>
                        <input type="checkbox" checked={a.echoCancel} onchange={(e) => h.setAudio({ echoCancel: e.currentTarget.checked })}>
                        <span class="neon-toggle"></span>
                    </label>
                    <label class="toggle-container call-switch">
                        <span>Noise suppression</span>
                        <input type="checkbox" checked={a.noiseSuppress} onchange={(e) => h.setAudio({ noiseSuppress: e.currentTarget.checked })}>
                        <span class="neon-toggle"></span>
                    </label>
                </div>
                <div class="call-panel-section">Connection</div>
                <div class="call-panel-grid">
                    <span class="call-panel-label">Quality</span>
                    <span class="call-panel-value">
                        {#if c.quality}<span class="call-dot call-dot-{c.quality}"></span> {QUALITY[c.quality]}{:else}Measuring…{/if}
                    </span>
                    <span class="call-panel-label">Delay</span>
                    <span class="call-panel-value">{delayMs == null ? '…' : `${delayMs} ms`}{#if path} <span class="call-panel-hint">{path}</span>{/if}</span>
                    <span class="call-panel-label" title="Moments the speaker had to fill in because the audio was late or missing">Lost audio</span>
                    <span class="call-panel-value">{lostText}</span>
                    <span class="call-panel-label" title="How much data your voice uses. It rises on a clean connection and drops when packets are being lost">Bitrate</span>
                    <span class="call-panel-value">{bitrate == null ? '…' : `${bitrate} kbit/s`}{#if bitrateHint} <span class="call-panel-hint">{bitrateHint}</span>{/if}</span>
                    {#if peerTrackLine('camera')}
                        <span class="call-panel-label">Their camera</span>
                        <span class="call-panel-value call-panel-value-quiet">{peerTrackLine('camera')}</span>
                    {/if}
                    {#if peerTrackLine('screen')}
                        <span class="call-panel-label">Their screen</span>
                        <span class="call-panel-value call-panel-value-quiet">{peerTrackLine('screen')}</span>
                    {/if}
                </div>
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="call-graph">
                    {#if graph}
                        <!-- svelte-ignore a11y_no_static_element_interactions -->
                        <div class="call-graph-plot" onpointermove={pick} onpointerdown={pick} onpointerleave={leave}>
                        <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-label="Last minute: delay as a line, lost audio as bars">
                            {#each graph.bars as b}
                                <rect x={b.x - graph.step / 2} y={H - 2 - b.hgt} width={Math.max(1.5, graph.step - 0.5)} height={b.hgt} class="call-graph-lost" />
                            {/each}
                            <polyline points={graph.line} class="call-graph-delay" />
                            {#if hover}
                                <path d="M{hover.x.toFixed(1)},{hover.y.toFixed(1)}h0.01" class="call-graph-dot" stroke-linecap="round" vector-effect="non-scaling-stroke" />
                            {/if}
                        </svg>
                        {#if hover}
                            <div class="call-graph-tip {tipSide}" style="left: {(hover.x / W * 100).toFixed(2)}%; top: {(hover.y / H * 100).toFixed(2)}%">
                                <b>{hover.rtt} ms</b> · {lostWord(hover.lost)} · {hover.ago === 0 ? 'now' : `${hover.ago}s ago`}
                            </div>
                        {/if}
                        </div>
                        <div class="call-graph-legend">
                            {#if hover}
                                <span class="call-graph-readout"><b>{hover.rtt} ms</b> · {lostWord(hover.lost)} · {hover.ago === 0 ? 'now' : `${hover.ago}s ago`}</span>
                            {:else}
                                <span><i class="call-graph-key call-graph-key-delay"></i>delay, up to {graph.maxRtt} ms</span>
                                <span><i class="call-graph-key call-graph-key-lost"></i>lost audio</span>
                            {/if}
                        </div>
                    {:else}
                        <div class="call-graph-empty">Measuring the connection…</div>
                    {/if}
                </div>
            </div>
        {/if}
    </div>
{/if}

<!-- No <style>: the .call-* rules live in styles.css with the rest of the chrome. -->
