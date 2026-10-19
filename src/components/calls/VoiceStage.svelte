<script>
    // A voice channel's stage, above its chat: who is in the call (speaking, muted, deafened),
    // and joining it, or the call's own controls once in.
    import { voiceSession, voicePresent, voiceStage, voiceCallHandlers, voiceLevel, voiceMicOff, toggleVoiceMic, SPEAKING } from '../lib/voice.svelte.js';
    import { openChatId } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    const stage = $derived(voiceStage());
    const shown = $derived(!!stage && openChatId() === stage.channelId);
    const h = $derived(voiceCallHandlers());
    const v = $derived(voiceSession());
    const micOff = $derived(voiceMicOff(v));
    const toggleMic = () => toggleVoiceMic(v, h);
    const inCall = $derived(!!v && !!stage && v.channelId === stage.channelId);
    const me = $derived(h.myNpub?.() || '');
    const people = $derived(stage ? voicePresent(stage.channelId) : []);
    let joining = $state(false);

    async function join() {
        if (joining || !stage) return;
        joining = true;
        try { await h.join(stage.communityId, stage.channelId); } finally { joining = false; }
    }
    const connected = (npub) => npub === me || !!v?.peers.find((p) => p.npub === npub && p.connected);
    // The call is as good as its slowest link: that round trip, once anyone is linked.
    const linked = $derived(inCall ? v.peers.filter((p) => p.connected && p.rtt_ms) : []);
    const ping = $derived(linked.length ? Math.max(...linked.map((p) => p.rtt_ms)) : 0);
    function pathOf(npub) {
        const p = inCall ? v.peers.find((x) => x.npub === npub && x.connected) : null;
        return p?.rtt_ms ? `${p.rtt_ms} ms, ${p.direct ? 'direct' : 'through a relay'}` : null;
    }
</script>

{#if shown}
    <div class="voice-stage" class:voice-stage-live={inCall}>
        <div class="voice-stage-head">
            <span class="icon icon-volume-max"></span>
            <span class="voice-stage-title cutoff">{stage.name}</span>
            {#if ping}
                <span class="voice-stage-ping" class:is-slow={ping >= 150} class:is-bad={ping >= 300} title="Round trip to the slowest link in the call">{ping} ms</span>
            {/if}
            <span class="voice-stage-count">{people.length ? `${people.length} in the call` : 'Nobody is here yet'}</span>
        </div>
        {#if people.length}
            <div class="voice-stage-grid">
                {#each people as p (p.npub)}
                    {@const level = inCall ? voiceLevel(p.npub, me) : 0}
                    <!-- svelte-ignore a11y_no_static_element_interactions -->
                    <div class="voice-tile" class:is-speaking={level > SPEAKING && !p.muted} class:is-pending={inCall && !connected(p.npub)}
                         title={p.elsewhere ? 'In this call on another app: you can\'t hear each other' : pathOf(p.npub)}
                         oncontextmenu={(e) => { e.preventDefault(); h.memberMenu?.(stage.communityId, stage.channelId, p.npub, e.clientX, e.clientY); }}>
                        <div class="voice-tile-avatar" style="--talk: {Math.min(1, level * 2).toFixed(2)}">
                            <Avatar src={h.avatarSrc?.(p.npub)} size={56} />
                            {#if p.muted || p.deafened}
                                {@const server = p.deafened ? p.server_deafened : p.server_muted}
                                <span class="voice-tile-badge" class:is-server={server} title={`${p.deafened ? 'Deafened' : 'Muted'}${server ? ' by a moderator' : ''}`}>
                                    <span class="icon" class:icon-volume-mute={p.deafened} class:icon-mic-off={!p.deafened}></span>
                                </span>
                            {/if}
                        </div>
                        <span class="voice-tile-name cutoff">{p.npub === me ? 'You' : h.getName?.(p.npub)}</span>
                        {#if p.elsewhere}<span class="voice-tile-note">another app</span>{:else if inCall && !connected(p.npub)}<span class="voice-tile-note">connecting</span>{/if}
                    </div>
                {/each}
            </div>
        {/if}
        <div class="voice-stage-actions">
            {#if inCall}
                {#if !v.mic}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <span class="voice-stage-warn" onclick={() => h.micAccess?.()}>No microphone access: others can't hear you</span>
                {/if}
                <button class="call-btn" class:call-btn-on={micOff} class:call-btn-locked={v.serverMuted}
                        title={v.serverMuted ? 'Muted by a moderator' : `${micOff ? 'Unmute' : 'Mute'} (${h.keyHint?.('M')})`} onclick={toggleMic}>
                    <span class="icon" class:icon-mic-off={micOff} class:icon-mic-on={!micOff}></span>
                </button>
                <button class="call-btn" class:call-btn-on={v.deafened || v.serverDeafened} class:call-btn-locked={v.serverDeafened}
                        title={v.serverDeafened ? 'Deafened by a moderator' : `${v.deafened ? 'Undeafen' : 'Deafen'} (${h.keyHint?.('D')})`} onclick={() => h.setDeafened(!(v.deafened || v.serverDeafened))}>
                    <span class="icon" class:icon-volume-mute={v.deafened || v.serverDeafened} class:icon-volume-max={!v.deafened && !v.serverDeafened}></span>
                </button>
                <button class="voice-stage-leave" onclick={() => h.leave()}>Leave</button>
            {:else}
                <button class="voice-stage-join" disabled={joining} onclick={join}>{joining ? 'Joining…' : 'Join Voice'}</button>
            {/if}
        </div>
    </div>
{/if}
