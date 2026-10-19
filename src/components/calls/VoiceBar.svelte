<script>
    // The voice Channel call this device is in, as the 1:1 call's pill: its community, how it's
    // going, and the same controls in the same order. It drags anywhere and shares the 1:1
    // pill's place; a tap opens the channel.
    import { voiceSession, voiceStage, voiceMicOff, toggleVoiceMic, voiceSince } from '../lib/voice.svelte.js';
    import { openChatId } from '../lib/signals.svelte.js';
    import { rttQuality } from '../lib/calls.svelte.js';
    import { loadPillPos, savePillPos, clampPill, pillDrag } from '../lib/call-pill.js';
    import { avatarThumb } from '../lib/avatar.js';
    import Avatar from '../ui/Avatar.svelte';
    /**
     * @typedef {object} VoiceBarHelpers
     * @property {(on: boolean) => void} setMuted
     * @property {(on: boolean) => void} setDeafened
     * @property {() => void} leave
     * @property {(on: boolean) => void} share
     * @property {(on: boolean) => void} camera
     * @property {() => boolean} canShare
     * @property {() => boolean} canCamera
     * @property {(key: string) => string} keyHint
     * @property {(communityId: string, channelId: string) => string} channelName
     * @property {(communityId: string) => string|null} communityAvatar   its cached avatar's path
     * @property {(path: string) => string} convertFileSrc
     * @property {(communityId: string, channelId: string) => void} openChannel
     */
    /** @type {{ h: VoiceBarHelpers }} */
    let { h } = $props();
    const v = $derived(voiceSession());
    const micOff = $derived(voiceMicOff(v));
    const toggleMic = () => toggleVoiceMic(v, h);
    // The call's own stage on screen carries the same controls.
    const onStage = $derived(!!v && voiceStage()?.channelId === v.channelId && openChatId() === v.channelId);
    const linked = $derived(v ? v.peers.filter((p) => p.connected) : []);
    // The call is as good as its slowest link.
    const ping = $derived(Math.max(0, ...linked.map((p) => p.rtt_ms || 0)));
    const quality = $derived(rttQuality(ping));
    const avatarPath = $derived(v ? h.communityAvatar(v.communityId) : null);
    const avatar = $derived(avatarPath ? h.convertFileSrc(avatarThumb(avatarPath)) : null);
    // The ring glows with whoever is loudest, on the 1:1 pill's curve.
    const talk = $derived(Math.min(1, Math.max(0, (Math.max(0, ...linked.map((p) => p.level || 0)) - 0.3) / 0.55)));

    let now = $state(Date.now());
    $effect(() => {
        if (!v) return;
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
    const status = $derived(!v ? '' : !v.audio ? 'Without audio'
        : `${linked.length ? `${linked.length + 1} in the call` : 'Waiting for others'} · ${clock(Math.max(0, now - voiceSince()))}`);

    let pos = $state(loadPillPos());
    let pill = $state(null);
    const clamp = (p) => clampPill(p, pill?.offsetWidth || 340, pill?.offsetHeight || 48);
    const dragger = pillDrag(() => pill, (p) => { pos = clamp(p); }, () => savePillPos(pos), () => h.openChannel(v.communityId, v.channelId));
    $effect(() => {
        const keep = () => { if (pos) pos = clamp(pos); };
        window.addEventListener('resize', keep);
        return () => window.removeEventListener('resize', keep);
    });
    const placement = $derived(pos ? `left:${pos.x}px; top:${pos.y}px; transform:none;` : '');
</script>

{#if v && !onStage}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="call-pill call-pill-live voice-bar" style={placement} bind:this={pill}>
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="call-pill-row" title="Open the channel" onpointerdown={dragger.down} onpointermove={dragger.move} onpointerup={dragger.up} onpointercancel={dragger.up}>
            <span class="call-pill-avatar" style="--talk: {talk.toFixed(2)}">
                <Avatar src={avatar} size={30} group />
            </span>
            <div class="call-pill-text">
                <span class="call-pill-name cutoff"><span class="icon icon-volume-max"></span>{h.channelName(v.communityId, v.channelId)}</span>
                <span class="call-pill-status cutoff">
                    {#if quality}<span class="call-dot call-dot-{quality}" title={`${ping} ms to the slowest link`}></span>{/if}
                    {status}
                </span>
            </div>
            {#if v.canVideo && h.canCamera()}
                <button class="call-btn" class:call-btn-on={v.camera} title={v.camera ? 'Turn your camera off' : 'Turn your camera on'} onclick={() => h.camera(!v.camera)}>
                    <span class="icon icon-video"></span>
                </button>
            {/if}
            {#if v.canVideo && h.canShare()}
                <button class="call-btn" class:call-btn-on={v.sharing} title={v.sharing ? 'Stop sharing your screen' : 'Share your screen'} onclick={() => h.share(!v.sharing)}>
                    <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" stroke="currentColor" stroke-width="2"/><path d="M8 20h8M12 16v4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
                </button>
            {/if}
            <button class="call-btn" class:call-btn-on={micOff} class:call-btn-locked={v.serverMuted}
                    title={v.serverMuted ? 'Muted by a moderator' : `${micOff ? 'Unmute' : 'Mute'} (${h.keyHint('M')})`} onclick={toggleMic}>
                <span class="icon" class:icon-mic-off={micOff} class:icon-mic-on={!micOff}></span>
            </button>
            <button class="call-btn" class:call-btn-on={v.deafened || v.serverDeafened} class:call-btn-locked={v.serverDeafened}
                    title={v.serverDeafened ? 'Deafened by a moderator' : `${v.deafened ? 'Undeafen' : 'Deafen'} (${h.keyHint('D')})`} onclick={() => h.setDeafened(!(v.deafened || v.serverDeafened))}>
                <span class="icon" class:icon-volume-mute={v.deafened || v.serverDeafened} class:icon-volume-max={!v.deafened && !v.serverDeafened}></span>
            </button>
            <button class="call-btn call-btn-hangup" title="Leave the call" onclick={() => h.leave()}>
                <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </button>
        </div>
    </div>
{/if}
