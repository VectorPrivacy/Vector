<script>
    // The Notifications section: mute toggles, content privacy, sound choice and the
    // custom-sound chip, derived from notif state. Writes go through `h` so the
    // desktop (settings blob) and mobile (per-key) persistence stay in the app.
    import { notifState } from '../lib/settings.svelte.js';
    let { h } = $props();   // h: saveSounds({globalMute, muteEveryone, sound}), saveMuteEveryone, savePrivacy, pickCustom, preview, explain(kind)

    const n = notifState();
    // The dropdown shows Custom while a file is still being picked, so it is local.
    let choice = $state('default');
    $effect(() => {
        const s = n.sound;
        choice = s.type === 'Custom' && s.path ? 'custom' : s.type === 'None' ? 'none' : s.type === 'Techno' ? 'techno' : 'default';
    });
    // "name_48000.raw" is the cache form; show the name the user picked.
    const customName = $derived(n.sound.path ? (n.sound.path.split(/[/\\]/).pop() || 'Unknown file').replace(/_\d+\.raw$/, '') : 'No file selected');

    function persistSounds() {
        h.saveSounds({ globalMute: n.globalMute, muteEveryone: n.muteEveryone, sound: n.sound });
    }
    function onMuteEveryone(e) {
        n.muteEveryone = e.target.checked;
        if (n.sounds) persistSounds(); else h.saveMuteEveryone(n.muteEveryone);
    }
    function onSound(e) {
        choice = e.target.value;
        if (choice === 'custom') {
            if (n.sound.path) { n.sound = { type: 'Custom', path: n.sound.path }; persistSounds(); }
            return;
        }
        n.sound = { type: choice === 'none' ? 'None' : choice === 'techno' ? 'Techno' : 'Default', path: n.sound.path };
        persistSounds();
    }
    async function pick() {
        const path = await h.pickCustom();
        if (!path) return;
        n.sound = { type: 'Custom', path };
        persistSounds();
    }
    function clearCustom(e) {
        e.stopPropagation();
        n.sound = { type: 'Default', path: null };
        persistSounds();
    }
    function info(kind) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(kind); };
    }
</script>

{#if n.sounds}
    <div class="form-group">
        <label class="toggle-container">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <span><span class="icon icon-info btn notif-info" onclick={info('mute')}></span>Mute All Notification Sounds</span>
            <input type="checkbox" checked={n.globalMute} onchange={(e) => { n.globalMute = e.target.checked; persistSounds(); }}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/if}

<div class="form-group">
    <label class="toggle-container">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span><span class="icon icon-info btn notif-info" onclick={info('everyone')}></span>Mute @Everyone Pings</span>
        <input type="checkbox" checked={n.muteEveryone} onchange={onMuteEveryone}>
        <span class="neon-toggle"></span>
    </label>
</div>

<div class="form-group" id="notif-privacy-group">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <span class="notif-privacy-label"><span class="icon icon-info btn notif-info" style="margin-right: 8px;" onclick={info('privacy')}></span>Content Privacy</span>
    <div class="select-container">
        <select id="notif-privacy-select" value={n.privacy} onchange={(e) => { n.privacy = e.target.value; h.savePrivacy(n.privacy); }}>
            <option value="full">Show sender and message</option>
            <option value="hide_content">Hide message</option>
            <option value="hide_all">Hide sender and message</option>
        </select>
    </div>
</div>

{#if n.sounds}
    <div class="form-group" style="display: flex; align-items: center; gap: 5px;">
        <div class="select-container" style="margin: 0; flex: 1">
            <select id="notif-sound-select" style="margin-bottom: 0 !important;" value={choice} onchange={onSound}>
                <option value="default">Prélude</option>
                <option value="techno">Techno</option>
                <option value="none">None</option>
                <option value="custom">Custom...</option>
            </select>
        </div>
        <button class="btn cancel-btn" style="margin: 0; padding: 0 10px; min-width: 42px; height: 42px; display: flex; align-items: center; justify-content: center;" title="Preview Sound" onclick={() => h.preview(n.sound)}>
            <img src="./icons/speaker_volume.svg" alt="Preview" style="width: 18px; height: 18px;">
        </button>
    </div>
    {#if choice === 'custom'}
        <div class="form-group">
            <div class="notif-custom-container">
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="notif-sound-chip" title="Click to change sound" onclick={pick}>
                    <span class="notif-sound-chip-icon">♪</span>
                    <span class="notif-sound-chip-name">{customName}</span>
                    <button class="notif-sound-chip-clear" title="Remove custom sound" onclick={clearCustom}>×</button>
                </div>
            </div>
        </div>
    {/if}
{/if}
