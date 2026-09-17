<script>
    // The Calls section: the three voice processing switches, applied at once, and a
    // microphone test that shows what the other side would hear.
    import { callAudio, callState } from '../lib/calls.svelte.js';
    import VoiceMeter from '../calls/VoiceMeter.svelte';
    let { h } = $props();   // h: setAudio(patch), micTestStart(), micTestStop()
    const a = callAudio();
    const c = callState();
    const inCall = $derived(!!c.id && c.phase !== 'ended');
    // Leaving the screen stops the test.
    $effect(() => () => { if (a.micTest) h.micTestStop(); });
</script>

<div class="form-group">
    <label class="toggle-container">
        <span>Automatic gain<span class="settings-hint">Lifts a quiet microphone so nobody has to shout</span></span>
        <input type="checkbox" checked={a.autoGain} onchange={(e) => h.setAudio({ autoGain: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group">
    <label class="toggle-container">
        <span>Echo cancellation<span class="settings-hint">Keeps your speaker out of your microphone</span></span>
        <input type="checkbox" checked={a.echoCancel} onchange={(e) => h.setAudio({ echoCancel: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group">
    <label class="toggle-container">
        <span>Noise suppression<span class="settings-hint">Fans, keyboards and hum, taken down</span></span>
        <input type="checkbox" checked={a.noiseSuppress} onchange={(e) => h.setAudio({ noiseSuppress: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>

<div class="form-group calls-mic-test">
    <div class="calls-mic-test-row">
        <span>Your voice<span class="settings-hint">{inCall ? 'Shown on the call while one is up' : a.micTest ? 'Speak normally: this is what they hear' : 'Test how you sound with the switches above'}</span></span>
        {#if !inCall}
            <button class="calls-mic-test-btn" class:calls-mic-test-on={a.micTest} onclick={() => a.micTest ? h.micTestStop() : h.micTestStart()}>
                {a.micTest ? 'Stop' : 'Test microphone'}
            </button>
        {/if}
    </div>
    <VoiceMeter level={inCall ? c.levels.mic : a.micLevel} active={inCall || a.micTest} />
</div>

<!-- No <style>: the .calls-* and .settings-hint rules live in styles.css. -->
