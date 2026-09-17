<script>
    // The call on screen: a ring card while a call comes in, otherwise a pill that
    // floats above every pane, drags anywhere, remembers where it was left, and opens
    // into volume, microphone and a plain-language view of the connection.
    import { untrack } from 'svelte';
    import { slide } from 'svelte/transition';
    import { callState, callAudio } from '../lib/calls.svelte.js';
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
    const OPEN_W = 300;
    let width = $state(null);
    let collapsedW = 0;
    $effect(() => {
        if (!expanded && width == null && pill) collapsedW = pill.offsetWidth;
    });
    function toggle() {
        if (!pill) { expanded = !expanded; return; }
        const from = pill.offsetWidth;
        if (!expanded) collapsedW = from;
        width = from;
        expanded = !expanded;
        requestAnimationFrame(() => requestAnimationFrame(() => { width = expanded ? OPEN_W : collapsedW; }));
    }
    function onWidthDone(e) {
        if (e.propertyName === 'width' && !expanded) width = null;
    }
    $effect(() => {
        const keep = () => { if (pos) pos = clamp(pos); };
        window.addEventListener('resize', keep);
        return () => window.removeEventListener('resize', keep);
    });
    // Opening the panel can push the pill off the bottom; pull it back in. The
    // position is read untracked: writing it from an effect that depends on it loops.
    $effect(() => {
        expanded;
        queueMicrotask(() => untrack(() => { if (pos) pos = clamp(pos); }));
    });
    const placement = $derived((pos ? `left:${pos.x}px; top:${pos.y}px; transform:none;` : '') + (width != null ? ` width:${width}px;` : ''));

    // ── the connection in words and a picture ──
    const delayMs = $derived(c.stats?.rtt_ms ?? null);
    const path = $derived(c.stats?.path === 'relay' ? 'via relay' : c.stats?.path === 'direct' ? 'direct' : '');
    const lostPct = $derived.by(() => {
        const recent = c.history.slice(-10);
        if (!recent.length) return 0;
        return recent.reduce((a, r) => a + r.lost, 0) / recent.length;
    });
    const lostText = $derived(lostPct < 0.05 ? 'none' : lostPct < 1 ? 'under 1%' : `${lostPct.toFixed(lostPct < 10 ? 1 : 0)}%`);
    // Graph: delay as a line, lost audio as bars, over the last minute.
    const W = 240, H = 44;
    const graph = $derived.by(() => {
        const hist = c.history;
        if (hist.length < 2) return null;
        const n = 60;
        const step = W / (n - 1);
        const off = n - hist.length;
        const maxRtt = Math.max(200, ...hist.map(r => r.rtt));
        const pts = hist.map((r, i) => ({ x: (off + i) * step, y: H - 2 - (r.rtt / maxRtt) * (H - 6), rtt: r.rtt, lost: r.lost, ago: hist.length - 1 - i }));
        const line = pts.map(p => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(' ');
        const bars = pts.map(p => ({ x: p.x, hgt: Math.min(H - 4, (p.lost / 10) * (H - 4)) })).filter(b => b.hgt > 0.5);
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
    const tipSide = $derived(!hover ? '' : hover.x < W * 0.2 ? 'call-graph-tip-left' : hover.x > W * 0.8 ? 'call-graph-tip-right' : '');
    function lostWord(v) { return v < 0.05 ? 'nothing lost' : v < 1 ? 'under 1% lost' : `${v.toFixed(v < 10 ? 1 : 0)}% lost`; }
</script>

{#if c.id && c.phase === 'ringing' && !c.outgoing}
    <div class="call-ring-overlay">
        <div class="call-ring-card">
            <Avatar src={peer?.avatar} size={84} />
            <div class="call-ring-name">{peer?.name || ''}</div>
            <div class="call-ring-sub">Incoming call</div>
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
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="call-pill" class:call-pill-open={expanded} class:call-pill-ended={c.phase === 'ended'} class:call-pill-live={live}
         style={placement} bind:this={pill} ontransitionend={onWidthDone}>
        <!-- Only the header row drags or toggles; the panel below is for its controls. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="call-pill-row" onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp} onpointercancel={onPointerUp}>
            <Avatar src={peer?.avatar} size={30} />
            <div class="call-pill-text">
                <span class="call-pill-name cutoff">{peer?.name || ''}</span>
                <span class="call-pill-status cutoff">
                    {#if live && c.quality}<span class="call-dot call-dot-{c.quality}" title={QUALITY[c.quality]}></span>{/if}
                    {status}{#if live && c.peerMuted} · muted{/if}
                </span>
            </div>
            {#if c.phase !== 'ended'}
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
        {#if expanded && c.phase !== 'ended'}
            <div class="call-panel" transition:slide={{ duration: 180 }}>
                <label class="call-panel-slider">
                    <span class="call-panel-label">Their volume</span>
                    <input type="range" min="0" max="200" step="5" value={Math.round(c.volume * 100)}
                           style="--slider-pct: {Math.round(c.volume * 50)}%"
                           oninput={(e) => h.setVolume(e.currentTarget.value / 100)} />
                    <span class="call-panel-value">{Math.round(c.volume * 100)}%</span>
                </label>
                <button class="call-panel-toggle" onclick={() => h.setMuted(!c.muted)}>
                    <span class="icon" class:icon-mic-off={c.muted} class:icon-mic-on={!c.muted}></span>
                    <span>{c.muted ? 'Microphone muted' : 'Microphone on'}</span>
                    <span class="call-panel-hint">{c.muted ? 'tap to unmute' : 'tap to mute'}</span>
                </button>
                <div class="call-panel-meters">
                    <span class="call-panel-label">Your voice</span>
                    <VoiceMeter level={c.muted ? 0 : c.levels.mic} active={live && !c.muted} />
                    <span class="call-panel-label">Their voice</span>
                    <VoiceMeter level={c.levels.peer} active={live} />
                </div>
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
                </div>
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="call-graph" onpointermove={pick} onpointerdown={pick} onpointerleave={leave}>
                    {#if graph}
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
                        <div class="call-graph-legend">
                            <span><i class="call-graph-key call-graph-key-delay"></i>delay, up to {graph.maxRtt} ms</span>
                            <span><i class="call-graph-key call-graph-key-lost"></i>lost audio</span>
                        </div>
                    {:else}
                        <div class="call-graph-empty">Measuring the connection…</div>
                    {/if}
                </div>
                <button class="call-panel-hangup" onclick={() => h.hangup()}>Hang up</button>
            </div>
        {/if}
    </div>
{/if}

<!-- No <style>: the .call-* rules live in styles.css with the rest of the chrome. -->
