<script>
    // The Calls section: the devices and their volumes, a microphone test that shows
    // what the other side would hear, then the voice processing switches. Every change
    // applies at once, mid-call included.
    import { callAudio, callState } from '../lib/calls.svelte.js';
    import VoiceMeter from '../calls/VoiceMeter.svelte';
    import Select from '../ui/Select.svelte';
    import InfoIcon from './InfoIcon.svelte';
    let { h } = $props();   // h: setAudio(patch), explain(kind), micTestStart(), micTestStop(), loadDevices(), setDevice(kind, name)
    const a = callAudio();
    const c = callState();
    const inCall = $derived(!!c.id && c.phase !== 'ended');
    const dev = $derived(a.devices);
    // The pickers only make sense where the OS exposes devices to pick from.
    const hasDevices = $derived(dev.inputs.length > 0 || dev.outputs.length > 0);
    // Leaving the screen stops the test.
    $effect(() => () => { if (a.micTest) h.micTestStop(); });
    $effect(() => { h.loadDevices(); });
    // '' is the system default: it follows the OS rather than naming a device.
    const byDefault = (name) => name ? `System Default - ${name}` : 'System Default';
    const inputs = $derived([{ value: '', label: byDefault(dev.defaultInput) },
        ...dev.inputs.map((name) => ({ value: name, label: name }))]);
    const outputs = $derived([{ value: '', label: byDefault(dev.defaultOutput) },
        ...dev.outputs.map((name) => ({ value: name, label: name }))]);
    const pick = (kind) => (v) => h.setDevice(kind, v === '' ? null : v);

    const SWITCHES = [
        { key: 'autoGain', label: 'Automatic Gain' },
        { key: 'echoCancel', label: 'Echo Cancellation' },
        { key: 'noiseSuppress', label: 'Noise Suppression' },
    ];
    const VOLUMES = [
        { key: 'micVolume', label: 'Microphone Volume' },
        { key: 'speakerVolume', label: 'Speaker Volume' },
    ];

    // A drag moves the slider every frame; the engine hears it a few times a second.
    let pending = null;
    let timer = null;
    function setVolume(key, v) {
        a[key] = v;
        pending = { ...pending, [key]: v };
        if (timer) return;
        timer = setTimeout(() => {
            const patch = pending;
            pending = null;
            timer = null;
            h.setAudio(patch);
        }, 80);
    }
    const pct = (v) => `${Math.round(v * 100)}%`;
</script>

{#snippet micLead()}<span class="icon icon-mic-on"></span>{/snippet}
{#snippet speakerLead()}<span class="icon icon-volume-max"></span>{/snippet}

{#if hasDevices}
    <div class="calls-pair">
        <div class="calls-field">
            <span class="calls-field-label">Microphone</span>
            <Select class="vselect-fill" options={inputs} value={dev.input ?? ''} lead={micLead} onchange={pick('input')} />
        </div>
        <div class="calls-field">
            <span class="calls-field-label">Speaker</span>
            <Select class="vselect-fill" options={outputs} value={dev.output ?? ''} lead={speakerLead} onchange={pick('output')} />
        </div>
    </div>
{/if}

<div class="calls-pair">
    {#each VOLUMES as v (v.key)}
        <label class="calls-field">
            <span class="calls-field-label">{v.label}</span>
            <input type="range" class="st-slider" min="0" max="100" step="1"
                   value={Math.round(a[v.key] * 100)} style:--slider-pct={pct(a[v.key])}
                   oninput={(e) => setVolume(v.key, e.currentTarget.valueAsNumber / 100)}>
        </label>
    {/each}
</div>

<div class="calls-mic-bench">
    {#if inCall}
        <span class="calls-mic-live">On a call</span>
    {:else}
        <button class="calls-mic-test-btn" onclick={() => a.micTest ? h.micTestStop() : h.micTestStart()}>
            {a.micTest ? 'Stop Test' : 'Test Mic'}
        </button>
    {/if}
    <VoiceMeter level={inCall ? c.levels.mic : a.micLevel} active={inCall || a.micTest} />
</div>

<p class="st-group">Microphone</p>
{#each SWITCHES as s (s.key)}
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.explain(s.key)} />{s.label}</span>
            <input type="checkbox" checked={a[s.key]} onchange={(e) => h.setAudio({ [s.key]: e.currentTarget.checked })}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/each}

<!-- No <style>: the .calls-* rules live in styles.css. -->
