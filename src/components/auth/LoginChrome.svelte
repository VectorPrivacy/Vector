<script>
    // Renderless: the login shell's screens, back bar, form classes, logo, and the bunker
    // overlay's status, link, copy button and busy state, painted from lib/login.svelte.js.
    // index.html keeps the markup; the flows never touch these elements directly.
    import { loginState, bunkerState } from '../lib/login.svelte.js';
    let { els } = $props();
    const l = loginState();
    const b = bunkerState();
    const show = (el, on) => { if (el) el.style.display = on ? '' : 'none'; };

    // ── screens ──
    $effect(() => {
        show(els.start, l.screen === 'start');
        show(els.import, l.screen === 'import');
        show(els.invite, l.screen === 'invite');
        show(els.welcome, l.screen === 'welcome');
        show(els.encrypt, l.screen === 'encrypt');
        // The welcome screen stands alone: the logo and tagline step aside for it.
        show(els.logo, l.screen !== 'welcome');
        show(els.subtext, l.screen !== 'welcome');
    });
    $effect(() => {
        show(els.form, l.shown);
        show(els.backBar, l.backBar);
        els.form.classList.toggle('has-back-bar', l.backBar);
        els.form.classList.toggle('bunker-active', l.bunker);
        els.bunker.classList.toggle('is-hidden', !l.bunker);
        show(els.bunker, l.bunker);
    });

    // ── bunker session ──
    // The countdown owns the status line while a link is live; a status write shows otherwise.
    const remaining = $derived(b.deadline ? Math.max(0, b.deadline - b.now) : 0);
    const line = $derived.by(() => {
        if (b.deadline && remaining > 0) {
            const secs = Math.ceil(remaining / 1000);
            return { text: `Waiting for signer… (${Math.floor(secs / 60)}:${(secs % 60).toString().padStart(2, '0')})`, kind: 'connecting' };
        }
        return { text: b.status, kind: b.kind };
    });
    $effect(() => {
        if (!els.status) return;
        els.status.textContent = line.text;
        els.status.className = 'login-bunker-status' + (line.kind ? ' ' + line.kind : '');
    });
    $effect(() => {
        if (els.qrWrap) els.qrWrap.classList.toggle('ready', b.qrReady);
        if (els.copy) {
            els.copy.disabled = !b.url || b.busy;
            els.copy.classList.toggle('copied', b.copied);
            els.copy.textContent = b.copied ? 'Copied — paste in your signer' : 'Copy connection link';
        }
        if (els.connect) els.connect.disabled = b.busy;
        if (els.urlInput) els.urlInput.disabled = b.busy;
        if (els.bunkerStart) els.bunkerStart.disabled = b.busy;
    });
</script>
