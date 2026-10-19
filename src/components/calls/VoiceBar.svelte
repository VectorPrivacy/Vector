<script>
    // The voice Channel call this device is in: where, how many, and its mute, deafen and leave.
    import { voiceSession, voiceStage } from '../lib/voice.svelte.js';
    import { openChatId } from '../lib/signals.svelte.js';
    let { h } = $props();   // h: setMuted(on), setDeafened(on), leave(), channelName(communityId, channelId), openChannel(communityId, channelId)
    const v = $derived(voiceSession());
    // The call's own stage on screen carries the same controls.
    const onStage = $derived(!!v && voiceStage()?.channelId === v.channelId && openChatId() === v.channelId);
    const others = $derived(v ? v.peers.filter((p) => p.connected).length : 0);
    const status = $derived(!v ? '' : !v.audio ? 'In the call, without audio'
        : others ? `${others + 1} in the call` : 'Waiting for others');
</script>

{#if v && !onStage}
    <div class="call-pill call-pill-live voice-bar">
        <div class="call-pill-row">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="call-pill-text" onclick={() => h.openChannel(v.communityId, v.channelId)}>
                <span class="call-pill-name cutoff"><span class="icon icon-volume-max"></span> {h.channelName(v.communityId, v.channelId)}</span>
                <span class="call-pill-status cutoff">{status}</span>
            </div>
            <button class="call-btn" class:call-btn-on={v.muted} title={v.muted ? 'Unmute' : 'Mute'} onclick={() => h.setMuted(!v.muted)}>
                <span class="icon" class:icon-mic-off={v.muted} class:icon-mic-on={!v.muted}></span>
            </button>
            <button class="call-btn" class:call-btn-on={v.deafened} title={v.deafened ? 'Undeafen' : 'Deafen'} onclick={() => h.setDeafened(!v.deafened)}>
                <span class="icon" class:icon-volume-mute={v.deafened} class:icon-volume-max={!v.deafened}></span>
            </button>
            <button class="call-btn call-btn-hangup" title="Leave the call" onclick={() => h.leave()}>
                <svg class="call-glyph-down" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </button>
        </div>
    </div>
{/if}
