<script>
    // A voice channel's stage, above its chat: who is in the call (speaking, muted, deafened),
    // and joining it, or the call's own controls once in.
    import { voiceSession, voicePresent, voiceStage, voiceCallHandlers, voiceLevel, voiceMicOff, toggleVoiceMic, voiceFocus, setVoiceFocus, voiceJoining, SPEAKING } from '../lib/voice.svelte.js';
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
    const joining = $derived(!!stage && voiceJoining() === stage.channelId);

    function join() {
        if (joining || !stage) return;
        h.join(stage.communityId, stage.channelId);
    }
    const connected = (npub) => npub === me || !!v?.peers.find((p) => p.npub === npub && p.connected);
    // The call is as good as its slowest link: that round trip, once anyone is linked.
    const linked = $derived(inCall ? v.peers.filter((p) => p.connected && p.rtt_ms) : []);
    const ping = $derived(linked.length ? Math.max(...linked.map((p) => p.rtt_ms)) : 0);
    // Shared screens: whose are live, and the one on the stage. Ours shows only when picked.
    const sharing = (p) => p.screen || (p.npub === me && !!v?.sharing);
    const sharers = $derived(people.filter(sharing).map((p) => p.npub));
    const focus = $derived(voiceFocus());
    const viewing = $derived(!inCall || focus === '' ? null
        : focus && sharers.includes(focus) ? focus : sharers.find((n) => n !== me) ?? null);
    const keyOf = (npub) => (inCall ? v.peers.find((p) => p.npub === npub && p.connected)?.pubkey : null) || null;
    const viewingKey = $derived(viewing && viewing !== me ? keyOf(viewing) : null);
    const cam = (p) => p.camera || (p.npub === me && !!v?.camera);
    let expanded = $state(false);
    // Only what the stage shows is sent to us: every picture we take costs its sender an upload.
    let watched = {};
    $effect(() => {
        const next = {};
        if (shown && inCall) {
            for (const p of people) {
                if (p.npub === me) continue;
                const want = { screen: viewing === p.npub, camera: !!p.camera };
                if (want.screen || want.camera) next[p.npub] = want;
            }
        }
        for (const npub of new Set([...Object.keys(watched), ...Object.keys(next)])) {
            const a = watched[npub], b = next[npub];
            if (a?.screen === b?.screen && a?.camera === b?.camera) continue;
            h.watch?.(npub, b || { screen: false, camera: false });
        }
        watched = next;
    });
    function pick(npub) {
        setVoiceFocus(viewing === npub ? '' : npub);
    }
    function videoCanvas(node, [key, kind]) {
        h.videoCanvas?.(key, kind, node);
        return { destroy() { h.videoCanvas?.(key, kind, node, true); } };
    }
    function selfPreview(node, kind) {
        h.selfPreview?.(kind, node);
        return { destroy() { h.selfPreview?.(kind, null); } };
    }
    function pathOf(npub) {
        const p = inCall ? v.peers.find((x) => x.npub === npub && x.connected) : null;
        return p?.rtt_ms ? `${p.rtt_ms} ms, ${p.direct ? 'direct' : 'through a relay'}` : null;
    }
</script>

{#if shown}
    <div class="voice-stage" class:voice-stage-live={inCall} class:voice-stage-expanded={expanded && !!viewing}>
        <div class="voice-stage-head">
            <span class="icon icon-volume-max"></span>
            <span class="voice-stage-title cutoff">{stage.name}</span>
            {#if ping}
                <span class="voice-stage-ping" class:is-slow={ping >= 150} class:is-bad={ping >= 300} title="Round trip to the slowest link in the call">{ping} ms</span>
            {/if}
            <span class="voice-stage-count">{people.length ? `${people.length} in the call` : 'Nobody is here yet'}</span>
        </div>
        {#if viewing}
            <div class="voice-screen">
                {#if viewing === me}
                    <video class="voice-screen-media" use:selfPreview={'screen'} muted playsinline autoplay></video>
                {:else if viewingKey}
                    {#key viewingKey}
                        <canvas class="voice-screen-media" use:videoCanvas={[viewingKey, 'screen']}></canvas>
                    {/key}
                {/if}
                <div class="voice-screen-bar">
                    <span class="voice-screen-label cutoff">{viewing === me ? 'Your screen' : `${h.getName?.(viewing)}'s screen`}</span>
                    <button class="voice-screen-btn" title={expanded ? 'Smaller' : 'Bigger'} onclick={() => (expanded = !expanded)}>
                        {#if expanded}
                            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                        {:else}
                            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                        {/if}
                    </button>
                    <button class="voice-screen-btn" title="Hide" onclick={() => setVoiceFocus('')}>
                        <span class="icon icon-x"></span>
                    </button>
                </div>
            </div>
        {/if}
        {#if people.length}
            <div class="voice-stage-grid">
                {#each people as p (p.npub)}
                    {@const level = inCall ? voiceLevel(p.npub, me) : 0}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div class="voice-tile" class:is-speaking={level > SPEAKING && !p.muted} class:is-pending={inCall && !connected(p.npub)}
                         class:is-sharing={sharing(p)} class:is-viewing={viewing === p.npub} class:has-cam={cam(p) && (p.npub === me || !!keyOf(p.npub))}
                         onclick={() => { if (inCall && sharing(p)) pick(p.npub); }}
                         title={p.elsewhere ? 'In this call on another app: you can\'t hear each other' : pathOf(p.npub)}
                         oncontextmenu={(e) => { e.preventDefault(); h.memberMenu?.(stage.communityId, stage.channelId, p.npub, e.clientX, e.clientY); }}>
                        <div class="voice-tile-avatar" class:voice-tile-cam={cam(p) && (p.npub === me || !!keyOf(p.npub))} style="--talk: {Math.min(1, level * 2).toFixed(2)}">
                            {#if cam(p) && p.npub === me}
                                <video class="voice-tile-video" use:selfPreview={'camera'} muted playsinline autoplay></video>
                            {:else if cam(p) && keyOf(p.npub)}
                                {#key keyOf(p.npub)}
                                    <canvas class="voice-tile-video" use:videoCanvas={[keyOf(p.npub), 'camera']}></canvas>
                                {/key}
                            {:else}
                                <Avatar src={h.avatarSrc?.(p.npub)} size={56} />
                            {/if}
                            {#if p.muted || p.deafened}
                                {@const server = p.deafened ? p.server_deafened : p.server_muted}
                                <span class="voice-tile-badge" class:is-server={server} title={`${p.deafened ? 'Deafened' : 'Muted'}${server ? ' by a moderator' : ''}`}>
                                    <span class="icon" class:icon-volume-mute={p.deafened} class:icon-mic-off={!p.deafened}></span>
                                </span>
                            {/if}
                        </div>
                        {#if sharing(p)}<span class="voice-tile-live" title={inCall ? (viewing === p.npub ? 'Hide their screen' : 'Show their screen') : 'Sharing their screen'}>LIVE</span>{/if}
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
                {#if v.sharing}
                    <span class="voice-stage-sharing">Sharing your screen{v.viewers ? ` · ${v.viewers} watching` : ''}</span>
                {/if}
                {#if h.canCamera?.()}
                    <button class="call-btn" class:call-btn-on={v.camera} title={v.camera ? 'Turn your camera off' : 'Turn your camera on'} onclick={() => h.camera?.(!v.camera)}>
                        <span class="icon icon-video"></span>
                    </button>
                {/if}
                {#if h.canShare?.()}
                    <button class="call-btn" class:call-btn-on={v.sharing} title={v.sharing ? 'Stop sharing your screen' : 'Share your screen'} onclick={() => h.share?.(!v.sharing)}>
                        <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" stroke="currentColor" stroke-width="2"/><path d="M8 20h8M12 16v4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
                    </button>
                {/if}
                <button class="voice-stage-leave" onclick={() => h.leave()}>Leave</button>
            {:else}
                <button class="voice-stage-join" disabled={joining} onclick={join}>{joining ? 'Joining…' : 'Join Voice'}</button>
            {/if}
        </div>
    </div>
{/if}
