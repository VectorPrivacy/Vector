<script>
    // Device transfer: one card for both sides. Either device shows a code (QR plus text) or takes
    // the other's (scan or type). The new device then shows a number and whose account is coming;
    // the signed-in one asks for that number, and the new device signs in once the account arrives.
    import { transferModal } from '../lib/transfer.svelte.js';
    import { credentialState } from '../lib/credential.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from '../ui/Avatar.svelte';
    let { h } = $props();   // h: TransferHelpers (js/transfer.js)
    const st = transferModal.state();
    const cred = credentialState();

    let now = $state(Date.now());
    $effect(() => {
        if (!st.active || st.stage !== 'show') return;
        now = Date.now();
        const id = setInterval(() => { now = Date.now(); }, 1000);
        return () => clearInterval(id);
    });
    const left = $derived(Math.max(0, Math.ceil((st.expiresAt - now) / 1000)));
    const clock = $derived(`${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`);

    const sender = $derived(st.role === 'sender');
    const title = $derived({
        match: 'Check the number',
        approve: 'Sign in on a new device?',
        finishing: 'Signing in',
        sent: st.unconfirmed ? 'Sent' : 'All set',
        error: 'Transfer stopped',
    }[st.stage] ?? (sender ? 'Sign in on another device' : 'Sign in with another device'));
    const closable = $derived(!st.busy && !['sending', 'finishing'].includes(st.stage));
    const shortNpub = $derived(st.sender ? `${st.sender.slice(0, 12)}…${st.sender.slice(-6)}` : '');
    const sasGroups = $derived(st.sas ? [st.sas.slice(0, 3), st.sas.slice(3)] : []);

    function qr(node, text) {
        const paint = (t) => { if (t) h.renderQr(node, t); };
        paint(text);
        return { update: paint };
    }
    function focus(node) { requestAnimationFrame(() => node.focus()); }
    function submitEntry(ev) { ev.preventDefault(); if (!st.busy && st.entry.trim()) h.connect(); }
    // Codes read "7-orbit-lemon-stage": lowercase, one dash between parts (Space types one), nothing
    // else. A pasted QR text loses its "Vector transfer code:" label.
    const cleanCode = (text) => text.replace(/^\s*vector transfer code:\s*/i, '').toLowerCase()
        .replace(/[^a-z0-9]+/g, '-').replace(/^-+/, '');
    function formatEntry(ev) {
        const el = ev.currentTarget;
        const caret = cleanCode(el.value.slice(0, el.selectionStart ?? el.value.length)).length;
        st.entry = cleanCode(el.value).slice(0, 40);
        el.value = st.entry;
        el.setSelectionRange(Math.min(caret, st.entry.length), Math.min(caret, st.entry.length));
    }
    function submitNumber(ev) { ev.preventDefault(); if (!st.busy && st.number.replace(/\D/g, '').length === 6) h.approve(); }
    // Six digits at most, shown as "123 456" however they were typed or pasted.
    function formatNumber(ev) {
        const digits = ev.currentTarget.value.replace(/\D/g, '').slice(0, 6);
        st.number = digits.length > 3 ? `${digits.slice(0, 3)} ${digits.slice(3)}` : digits;
        ev.currentTarget.value = st.number;
    }
</script>

<!-- Escape belongs to the PIN prompt while it's up over this card. -->
<svelte:window onkeydown={(e) => {
    if (e.key === 'Escape' && !e.defaultPrevented && !cred.open && st.active && !st.closing && closable) h.close();
}} />

{#snippet identity()}
    <div class="xfer-id">
        <Avatar src={st.avatar || null} size={44} />
        <div class="xfer-id-text">
            <span class="xfer-id-name">{st.name || 'Unnamed account'}</span>
            {#if shortNpub}<span class="xfer-id-npub">{shortNpub}</span>{/if}
        </div>
    </div>
{/snippet}

{#snippet spinner(text)}
    <div class="xfer-wait" role="status"><span class="xfer-spin" aria-hidden="true"></span>{text}</div>
{/snippet}

<div class="vcard-overlay xfer-overlay" class:active={st.active} class:closing={st.closing}>
    <div class="vcard xfer-card" role="dialog" aria-modal="true" aria-labelledby="xfer-title" use:popIn={st.tick}>
        <div class="vcard-head">
            <span class="vcard-badge xfer-badge" aria-hidden="true"></span>
            <h3 id="xfer-title" class="vcard-title">{title}</h3>
            {#if closable}
                <button type="button" class="vcard-x" aria-label="Close" onclick={() => h.close()}>&#x2715;</button>
            {/if}
        </div>

        {#if st.stage === 'pick'}
            <p class="vcard-lead">On your signed-in device, open Settings and choose Sign in on Another Device. Then scan the QR code it shows.</p>
            <div class="vcard-actions">
                <button type="button" class="vcard-btn xfer-scan-btn" onclick={() => h.scan()}>
                    <span class="xfer-scan-glyph" aria-hidden="true"></span>Scan QR Code
                </button>
                <button type="button" class="vcard-btn ghost" onclick={() => h.enterCode()}>Use a Text Code Instead</button>
                <button type="button" class="xfer-link" onclick={() => h.showCode()}>Show a code on this device instead</button>
            </div>
        {:else if st.stage === 'show'}
            <p class="vcard-lead">
                {sender
                    ? 'On the new device, tap Login, then Sign in with another device, and scan this or type the code.'
                    : 'On your signed-in device, open Settings and choose Sign in on another device, then scan this or type the code.'}
            </p>
            <div class="xfer-qr-tile"><div class="xfer-qr" aria-label="Transfer QR code" use:qr={st.qr}></div></div>
            <div class="xfer-code">
                <span class="xfer-code-text">{st.code}</span>
                <span class="xfer-expiry" class:low={left <= 30}>{left ? `Expires in ${clock}` : 'Expired'}</span>
            </div>
            <div class="vcard-actions">
                {#if st.canScan}
                    <button type="button" class="vcard-btn ghost" onclick={() => h.scan()}>Scan the other device instead</button>
                {/if}
                <button type="button" class="xfer-link" onclick={() => h.enterCode()}>Type the other device's code instead</button>
            </div>
        {:else if st.stage === 'enter'}
            <p class="vcard-lead">Type the code shown on your other device. It's a number and three words.</p>
            <form class="xfer-entry" onsubmit={submitEntry}>
                <input use:focus value={st.entry} oninput={formatEntry} class="xfer-input" placeholder="7-orbit-lemon-stage" maxlength="64"
                       autocomplete="off" autocapitalize="none" autocorrect="off" spellcheck="false" disabled={st.busy}>
                {#if st.error}<p class="xfer-error" role="alert">{st.error}</p>{/if}
                <div class="vcard-actions">
                    <button type="submit" class="vcard-btn" disabled={st.busy || !st.entry.trim()}>{st.busy ? 'Connecting…' : 'Connect'}</button>
                    {#if st.canScan}
                        <button type="button" class="vcard-btn ghost" disabled={st.busy} onclick={() => h.scan()}>Scan instead</button>
                    {/if}
                    <button type="button" class="xfer-link" disabled={st.busy} onclick={() => h.showCode()}>Show a code on this device instead</button>
                </div>
            </form>
        {:else if st.stage === 'connecting'}
            {@render spinner('Connecting…')}
        {:else if st.stage === 'match'}
            <p class="vcard-lead xfer-center">On your other device, type this number to approve.</p>
            <div class="xfer-sas" role="img" aria-label={`Number ${st.sas.split('').join(' ')}`}>
                {#each sasGroups as group}<span class="xfer-sas-group">{#each group.split('') as digit}<span>{digit}</span>{/each}</span>{/each}
            </div>
            {#if st.sender}
                <p class="xfer-signing-as">Signing in as</p>
                {@render identity()}
            {/if}
            <div class="vcard-actions">
                <button type="button" class="vcard-btn ghost" onclick={() => h.close()}>Cancel</button>
            </div>
        {:else if st.stage === 'approve'}
            <p class="vcard-lead">
                You're giving full, permanent control of this account to another device.
                <span class="keys-warn">Only continue if you're setting up your own device right now.</span>
                Vector never asks for this to verify, unlock or join anything.
            </p>
            <form class="xfer-entry" onsubmit={submitNumber}>
                <label class="vcard-label" for="xfer-number">Number shown on the new device</label>
                <input id="xfer-number" use:focus value={st.number} oninput={formatNumber} class="xfer-input xfer-number" placeholder="000 000"
                       inputmode="numeric" maxlength="7" autocomplete="off" disabled={st.busy}>
                {#if st.error}<p class="xfer-error" role="alert">{st.error}</p>{/if}
                <div class="vcard-actions">
                    <button type="submit" class="vcard-btn" disabled={st.busy || st.number.replace(/\D/g, '').length !== 6}>Approve</button>
                    <button type="button" class="vcard-btn ghost" disabled={st.busy} onclick={() => h.deny()}>Deny</button>
                </div>
            </form>
        {:else if st.stage === 'sending'}
            {@render spinner('Sending your account…')}
        {:else if st.stage === 'sent'}
            <p class="vcard-lead">
                {st.unconfirmed
                    ? "Your account is on its way. Check the new device to finish signing in."
                    : 'Your new device is signing in. Your chats and communities will sync there on their own.'}
            </p>
            <div class="vcard-actions">
                <button type="button" class="vcard-btn" onclick={() => h.close()}>Done</button>
            </div>
        {:else if st.stage === 'finishing'}
            {#if st.sender}{@render identity()}{/if}
            {@render spinner('Signing in…')}
        {:else if st.stage === 'error'}
            <p class="vcard-lead">{st.error}</p>
            <div class="vcard-actions">
                <button type="button" class="vcard-btn" onclick={() => h.retry()}>Try Again</button>
                <button type="button" class="vcard-btn ghost" onclick={() => h.close()}>Close</button>
            </div>
        {/if}
    </div>
</div>
