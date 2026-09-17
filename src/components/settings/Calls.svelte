<script>
    // The Calls section: the three voice processing switches, applied at once, and a
    // microphone test that shows what the other side would hear.
    import { callAudio, callState } from '../lib/calls.svelte.js';
    import VoiceMeter from '../calls/VoiceMeter.svelte';
    let { h } = $props();   // h: setAudio(patch), micTestStart(), micTestStop(), loadDevices(), setDevice(kind, name)
    const a = callAudio();
    const c = callState();
    const inCall = $derived(!!c.id && c.phase !== 'ended');
    const dev = $derived(a.devices);
    // The pickers only make sense where the OS exposes devices to pick from.
    const hasDevices = $derived(dev.inputs.length > 0 || dev.outputs.length > 0);
    // Leaving the screen stops the test.
    $effect(() => () => { if (a.micTest) h.micTestStop(); });
    $effect(() => { h.loadDevices(); });
    const pick = (kind) => (e) => h.setDevice(kind, e.currentTarget.value === '' ? null : e.currentTarget.value);
</script>

<div class="form-group">
    <label class="toggle-container">
        <span class="settings-label">Automatic gain<span class="settings-hint">Lifts a quiet microphone so nobody has to shout</span></span>
        <input type="checkbox" checked={a.autoGain} onchange={(e) => h.setAudio({ autoGain: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group">
    <label class="toggle-container">
        <span class="settings-label">Echo cancellation<span class="settings-hint">Keeps your speaker out of your microphone</span></span>
        <input type="checkbox" checked={a.echoCancel} onchange={(e) => h.setAudio({ echoCancel: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group">
    <label class="toggle-container">
        <span class="settings-label">Noise suppression<span class="settings-hint">Fans, keyboards and hum, taken down</span></span>
        <input type="checkbox" checked={a.noiseSuppress} onchange={(e) => h.setAudio({ noiseSuppress: e.currentTarget.checked })}>
        <span class="neon-toggle"></span>
    </label>
</div>

{#if hasDevices}
    <div class="form-group calls-device-row">
        <label for="calls-mic-device" class="settings-label">Microphone<span class="settings-hint">System default follows the OS; a named device is used whenever it is plugged in</span></label>
        <select id="calls-mic-device" class="form-control" value={dev.input ?? ''} onchange={pick('input')}>
            <option value="">System default{dev.defaultInput ? ` (${dev.defaultInput})` : ''}</option>
            {#each dev.inputs as name (name)}
                <option value={name}>{name}</option>
            {/each}
        </select>
    </div>
    <div class="form-group calls-device-row">
        <label for="calls-speaker-device" class="settings-label">Speaker</label>
        <select id="calls-speaker-device" class="form-control" value={dev.output ?? ''} onchange={pick('output')}>
            <option value="">System default{dev.defaultOutput ? ` (${dev.defaultOutput})` : ''}</option>
            {#each dev.outputs as name (name)}
                <option value={name}>{name}</option>
            {/each}
        </select>
    </div>
{/if}

<div class="form-group calls-mic-test">
    <div class="calls-mic-test-row">
        <span class="settings-label">Your voice<span class="settings-hint">{inCall ? 'Shown on the call while one is up' : a.micTest ? 'Speak normally: this is what they hear' : 'Test how you sound with the switches above'}</span></span>
        {#if !inCall}
            <button class="calls-mic-test-btn" class:calls-mic-test-on={a.micTest} onclick={() => a.micTest ? h.micTestStop() : h.micTestStart()}>
                {a.micTest ? 'Stop' : 'Test microphone'}
            </button>
        {/if}
    </div>
    <VoiceMeter level={inCall ? c.levels.mic : a.micLevel} active={inCall || a.micTest} />
</div>

<!-- No <style>: the .calls-* and .settings-hint rules live in styles.css. -->
