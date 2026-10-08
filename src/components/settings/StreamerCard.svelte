<script>
    // Streamer Mode: the card that turns it on, then whether we allow other people's streams
    // to show us (a field of our own profile) and how much more notifications hide while live.
    import { streamerState } from '../lib/streamer.svelte.js';
    import { notifState } from '../lib/settings.svelte.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import InfoIcon from './InfoIcon.svelte';
    import Select from '../ui/Select.svelte';

    let { h } = $props();   // h: StreamerHelpers (js/settings.js)

    const st = streamerState();
    const notif = notifState();
    let busy = $state(false);
    let consentBusy = $state(false);

    const consent = $derived.by(() => { profileVersion(h.myNpub() || ''); return h.consent(); });

    // Notification privacy as two bits: 1 hides the sender, 2 hides the message.
    const BITS = { full: 0, none: 0, hide_sender: 1, hide_content: 2, hide_all: 3 };
    const LEVELS = [
        { value: 'none', label: 'Nothing extra' },
        { value: 'hide_content', label: 'Messages' },
        { value: 'hide_sender', label: 'Names' },
        { value: 'hide_all', label: 'Names and messages' },
    ];
    const base = $derived(BITS[notif.privacy] || 0);
    // An option the base setting already covers would add nothing.
    const levels = $derived(LEVELS.map((o) => ({ ...o, disabled: o.value !== 'none' && (BITS[o.value] & ~base) === 0 })));
    const caption = $derived.by(() => {
        const bits = base | (BITS[st.notif] || 0);
        return `Notifications while streaming: ${bits & 1 ? 'name hidden' : 'name shown if they allow streams'}, ${bits & 2 ? 'message hidden' : 'message shown'}`;
    });

    // The box shows what stuck once the backend answers.
    async function toggleMode(e) {
        const box = e.currentTarget;
        busy = true;
        await h.setMode(box.checked);
        busy = false;
        box.checked = st.on;
    }
    async function toggleWallpapers(e) {
        const box = e.currentTarget;
        await h.setWallpapers(box.checked);
        box.checked = st.hideWallpapers;
    }
    async function toggleConsent(e) {
        const box = e.currentTarget;
        consentBusy = true;
        box.checked = await h.setConsent(box.checked);
        consentBusy = false;
    }
</script>

<div class="form-group tor-card streamer-card" class:is-live={st.on}>
    <div class="tor-glyph-wrap streamer-glyph"><span class="icon icon-video"></span></div>
    <div class="tor-card-body">
        <div class="tor-card-title">
            Streamer Mode<InfoIcon onclick={() => h.help('streamerMode')} />
        </div>
    </div>
    <label class="toggle-container tor-card-toggle">
        <input type="checkbox" aria-label="Streamer Mode" checked={st.on} disabled={busy} onchange={toggleMode}>
        <span class="neon-toggle"></span>
    </label>
</div>

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('streamConsent')} />Show me on streams</span>
        <input type="checkbox" checked={consent} disabled={consentBusy} onchange={toggleConsent}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('streamerWallpapers')} />Hide chat wallpapers</span>
        <input type="checkbox" checked={st.hideWallpapers} onchange={toggleWallpapers}>
        <span class="neon-toggle"></span>
    </label>
</div>
<div class="form-group st-line">
    <InfoIcon side="right" flex onclick={() => h.help('streamerNotif')} />
    <span class="st-line-label">While streaming, also hide</span>
    <Select class="streamer-select" label="While streaming, also hide" options={levels} value={st.notif} onchange={(v) => h.setNotif(v)} />
</div>
<p class="streamer-caption">{caption}</p>
