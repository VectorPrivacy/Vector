<script>
    // The call on screen: a ring card while a call comes in, otherwise a floating bar
    // that outlives any navigation, so a call never disappears behind a screen change.
    import { callState } from '../lib/calls.svelte.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    /**
     * @typedef {object} CallHelpers
     * @property {() => void} accept
     * @property {() => void} reject
     * @property {() => void} hangup
     * @property {(on: boolean) => void} setMuted
     * @property {(npub: string) => object|null} getProfile
     * @property {(profileOrId: object|string) => string} getName
     * @property {(profile: object|null) => string|null} getProfileAvatarSrc
     * @property {(npub: string) => void} openChat
     */
    /** @type {{ h: CallHelpers }} */
    let { h } = $props();
    const c = callState();

    const REASONS = {
        hangup: 'Call ended', rejected: 'Declined', busy: 'Busy', no_answer: 'No answer',
        missed: 'Missed call', connect_failed: 'Could not connect', disconnected: 'Connection lost',
        audio_failed: 'Microphone unavailable', send_failed: 'Could not reach the relays',
        account_changed: 'Call ended',
    };

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
    const line = $derived.by(() => {
        if (c.phase !== 'active' || !c.stats) return c.peerMuted ? 'muted' : '';
        const bits = [`${c.stats.rtt_ms} ms`, c.stats.path];
        // Concealment is the only counter you can hear; a percent under one is noise.
        const pct = c.stats.received ? Math.round(100 * c.stats.concealed / c.stats.received) : 0;
        if (pct >= 1) bits.push(`${pct}% concealed`);
        if (c.peerMuted) bits.push('muted');
        return bits.join(' · ');
    });
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
    <div class="call-bar" class:call-bar-ended={c.phase === 'ended'} class:call-bar-active={c.phase === 'active'}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="call-bar-peer btn" onclick={() => c.peer && h.openChat(c.peer)}>
            <Avatar src={peer?.avatar} size={30} />
            <span class="call-bar-text">
                <span class="call-bar-name cutoff">{peer?.name || ''}</span>
                <span class="call-bar-status cutoff">{status}{line ? ` · ${line}` : ''}</span>
            </span>
        </span>
        {#if c.phase !== 'ended'}
            <button class="call-bar-btn" class:call-bar-btn-on={c.muted} title={c.muted ? 'Unmute' : 'Mute'} onclick={() => h.setMuted(!c.muted)}>
                <span class="icon" class:icon-mic-off={c.muted} class:icon-mic-on={!c.muted}></span>
            </button>
            <button class="call-bar-btn call-bar-hangup" title="Hang up" onclick={() => h.hangup()}>
                <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
            </button>
        {/if}
    </div>
{/if}

<!-- No <style>: the .call-* rules live in styles.css with the rest of the chrome. -->
