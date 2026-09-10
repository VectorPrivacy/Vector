<script>
    // The login screen inside #login-form: the back bar, the pre-login account picker, the
    // start / import / bunker / invite / welcome / encrypt screens. Painted from
    // lib/login.svelte.js; every control answers through `h` (js/auth.js).
    import { untrack } from 'svelte';
    import { loginState, bunkerState, pickerState, encryptState } from '../lib/login.svelte.js';
    import PinInput from '../ui/PinInput.svelte';
    import AccountRows from '../people/AccountRows.svelte';
    import Avatar from '../ui/Avatar.svelte';

    let { h } = $props();   // h: LoginHelpers (js/auth.js)
    const l = loginState();
    const b = bunkerState();
    const p = pickerState();
    const e = encryptState();

    // ── account picker ──
    let pill = $state(null);
    let list = $state(null);
    // The list hangs off the pill's bottom edge wherever the form lays it out.
    $effect(() => {
        if (p.open && pill && list) list.style.top = `${Math.round(pill.getBoundingClientRect().bottom + 8)}px`;
    });

    // ── bunker ──
    // The countdown owns the status line while a link is live; a status write shows otherwise.
    const remaining = $derived(b.deadline ? Math.max(0, b.deadline - b.now) : 0);
    const line = $derived.by(() => {
        if (b.deadline && remaining > 0) {
            const secs = Math.ceil(remaining / 1000);
            return { text: `Waiting for signer… (${Math.floor(secs / 60)}:${(secs % 60).toString().padStart(2, '0')})`, kind: 'connecting' };
        }
        return { text: b.status, kind: b.kind };
    });
    function qr(node, url) {
        const paint = (u) => {
            if (!u) { node.replaceChildren(); delete node.dataset.qrKey; b.qrReady = false; return; }
            b.qrReady = !!h.bunker.renderQr(node, u);
        };
        paint(url);
        return { update: paint };
    }

    // ── encrypt ──
    let pinRow = $state(null);
    let passwordEl = $state(null);
    $effect(() => { e.pinTick; untrack(() => pinRow?.clear(e.pinFocus)); });
    $effect(() => {
        e.focusTick;
        untrack(() => {
            if (e.pinShown) pinRow?.focusFirst();
            else if (e.passwordShown) passwordEl?.focus();
        });
    });
    function submitPassword(ev) { ev.preventDefault(); h.encrypt.submitPassword(); }
</script>

{#if l.backBar}
    <div id="login-back-bar" class="chat-new-header">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="login-back-btn" class="btn chat-new-back-text-btn" onclick={() => h.back()}>
            <span class="icon icon-chevron-double-left nav-icon"></span>
            <p class="chat-new-back-text">Back</p>
        </div>
    </div>
{/if}

{#if p.shown}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="login-account-picker" class="login-account-picker" class:open={p.open} bind:this={pill} onclick={() => h.picker.toggle()}>
        <Avatar src={p.avatar} size={36} />
        <span id="login-account-picker-name" class="login-account-picker-name">{p.label}</span>
        <svg class="login-account-picker-chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M6 9l6 6 6-6" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
    </div>
{/if}
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="login-account-list-backdrop" class="login-account-list-backdrop" class:visible={p.open} onclick={() => h.picker.close()}></div>
<div id="login-account-list" class="login-account-list" class:open={p.open} role="dialog" aria-label="Pick account" bind:this={list}>
    {#if p.open}
        <!-- The active account stays in the pill; only the alternates are rows. -->
        <AccountRows accounts={p.accounts.filter(m => m.npub !== p.activeNpub)} h={h.picker.rowHelpers()} onPick={(m) => h.picker.pick(m)} />
    {/if}
</div>

{#if l.screen !== 'welcome'}
    <img src="./icons/vector-logo.svg" class="login-logo" alt="">
    <h4 class="startup-subtext-gradient login-subtext">Private & Encrypted Messenger</h4>
{/if}

{#if l.screen === 'start'}
    <div id="login-start">
        <button id="start-account-creation-btn" class="login-create-btn" onclick={() => h.createAccount()}>Create Account</button>
        <br>
        <button id="start-login-btn" class="login-login-btn" onclick={() => h.openImport()}>Login</button>
        <br>
        <img src="./icons/by-formlesslabs.svg" class="login-credits" alt="">
    </div>
{:else if l.screen === 'import'}
    <div id="login-import" class="login-import-container">
        <img src="./icons/by-formlesslabs.svg" class="login-credits" alt="">
        <div class="row input-box login-input-container">
            <div class="row chat-input-container">
                <input type="password" class="login-input" id="login-input" placeholder="Enter nsec or Seed Phrase..." bind:value={l.importKey} />
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <svg id="login-btn" class="btn login-btn" width="24" height="24" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" onclick={() => h.importKey()}>
                    <path d="M15 3H16.2C17.8802 3 18.7202 3 19.362 3.32698C19.9265 3.6146 20.3854 4.07354 20.673 4.63803C21 5.27976 21 6.11985 21 7.8V16.2C21 17.8802 21 18.7202 20.673 19.362C20.3854 19.9265 19.9265 20.3854 19.362 20.673C18.7202 21 17.8802 21 16.2 21H15M10 7L15 12M15 12L10 17M15 12L3 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </div>
        </div>
        <div class="login-signer-links">
            <button id="start-bunker-btn" class="login-bunker-link-btn" type="button" disabled={b.busy} onclick={() => h.bunker.open()}>
                <img src="./icons/key.svg" class="login-bunker-link-icon" alt="">
                <span>Use a Remote Signer</span>
            </button>
            {#if l.nip55Shown}
                <button id="start-nip55-btn" class="login-bunker-link-btn" type="button" disabled={l.nip55Busy} onclick={() => h.nip55()}>
                    <img src="./icons/key.svg" class="login-bunker-link-icon" alt="">
                    <span>Sign in with Amber (Offline)</span>
                </button>
            {/if}
        </div>
    </div>
{:else if l.screen === 'invite'}
    <div id="login-invite" class="login-invite-container">
        <div class="login-invite-header">
            <h3 class="login-invite-title">Enter Invite Code</h3>
        </div>
        <p class="login-invite-description">Enter your invite code to join Vector Beta</p>
        <div class="row input-box login-input-container">
            <div class="row chat-input-container">
                <!-- svelte-ignore a11y_autofocus -->
                <input type="text" class="login-input" id="invite-input" placeholder="Invite code..." autofocus bind:value={l.inviteCode}
                       onkeydown={(ev) => { if (ev.code === 'Enter' || ev.code === 'NumpadEnter') { ev.preventDefault(); h.invite(); } }} />
                <button id="invite-btn" class="btn login-btn" aria-label="Submit invite code" onclick={() => h.invite()}>
                    <svg width="100%" height="100%" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <path d="M15 3H16.2C17.8802 3 18.7202 3 19.362 3.32698C19.9265 3.6146 20.3854 4.07354 20.673 4.63803C21 5.27976 21 6.11985 21 7.8V16.2C21 17.8802 21 18.7202 20.673 19.362C20.3854 19.9265 19.9265 20.3854 19.362 20.673C18.7202 21 17.8802 21 16.2 21H15M10 7L15 12M15 12L10 17M15 12L3 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            </div>
        </div>
    </div>
{:else if l.screen === 'welcome'}
    <div id="login-welcome" class="login-welcome-container">
        <div class="login-welcome-content">
            <span class="icon icon-vector-check login-welcome-icon"></span>
            <h1 class="login-welcome-title" style="font-size: 20px; margin-bottom: 4px; color: #33db98;">Welcome to</h1>
            <h1 class="login-welcome-title">Vector Beta!</h1>
            <p class="login-welcome-subtitle">Congratulations! You're now part of the exclusive Vector Beta community.</p>
            <p class="login-welcome-footer">Setting up your account...</p>
        </div>
    </div>
{:else if l.screen === 'encrypt'}
    <div id="login-encrypt" class="login-encrypt-container">
        {#if e.headerShown}
            <div class="login-encrypt-header">
                {#if e.lockShown}<img src="./icons/lock.svg" class="login-lock-icon" alt="">{/if}
                <h3 id="login-encrypt-title" class="login-encrypt-title" class:startup-subtext-gradient={e.gradient} class:typing-indicator-text={e.typing} style:color={e.error ? 'red' : ''}>{e.title}</h3>
            </div>
        {/if}
        {#if e.typeSelectShown}
            <div id="login-encrypt-type-select">
                <div class="security-type-options">
                    <button id="security-type-skip" class="security-type-option-btn" onclick={() => h.encrypt.choose('skip')}>Skip Encryption</button>
                    <button id="security-type-password" class="security-type-option-btn" onclick={() => h.encrypt.choose('password')}>Create Password</button>
                    <p class="security-type-recommended">{e.recommended}</p>
                    <button id="security-type-pin" class="security-type-option-btn accent" onclick={() => h.encrypt.choose('pin')}>Create PIN</button>
                    {#if e.bioOptionShown}
                        <button id="security-type-biometric" class="security-type-option-btn accent" onclick={() => h.encrypt.choose('biometric')}>{e.bioOptionLabel}</button>
                    {/if}
                </div>
                <img src="./icons/by-formlesslabs.svg" class="login-credits" alt="">
            </div>
        {/if}
        {#if e.pinShown}
            <PinInput bind:this={pinRow} id="login-encrypt-pins" cls="row pin-row input-box" inputIds={['pin-0', 'pin-1', 'pin-2', 'pin-3', 'pin-4', 'pin-5']}
                    onFull={(pin) => h.encrypt.pinFull(pin)} onBackspace={() => h.encrypt.pinBackspace()} />
        {/if}
        {#if e.passwordShown}
            <div id="login-encrypt-password" class="login-encrypt-password">
                <div class="row input-box login-input-container">
                    <div class="row chat-input-container">
                        <!-- svelte-ignore a11y_autofocus -->
                        <input type="password" id="login-password-input" class="login-input" placeholder="Enter your password" autocomplete="off" autofocus
                               bind:this={passwordEl} bind:value={e.password} onkeydown={(ev) => { if (ev.key === 'Enter') submitPassword(ev); }}>
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <svg id="login-password-btn" class="btn login-btn" width="24" height="24" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" onclick={submitPassword}>
                            <path d="M15 3H16.2C17.8802 3 18.7202 3 19.362 3.32698C19.9265 3.6146 20.3854 4.07354 20.673 4.63803C21 5.27976 21 6.11985 21 7.8V16.2C21 17.8802 21 18.7202 20.673 19.362C20.3854 19.9265 19.9265 20.3854 19.362 20.673C18.7202 21 17.8802 21 16.2 21H15M10 7L15 12M15 12L10 17M15 12L3 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                        </svg>
                    </div>
                </div>
            </div>
        {/if}
        {#if e.bioBtnShown}
            <button id="login-biometric-btn" class="security-type-option-btn" style="margin-top: 18px; margin-bottom: calc(28px + env(safe-area-inset-bottom, 0px));" onclick={() => h.encrypt.biometric()}>
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" style="vertical-align: -4px; margin-right: 8px;">
                    <path d="M5.80688 18.5304C5.82459 18.5005 5.84273 18.4709 5.8613 18.4413C7.2158 16.2881 7.99991 13.7418 7.99991 11C7.99991 8.79086 9.79077 7 11.9999 7C14.209 7 15.9999 8.79086 15.9999 11C15.9999 12.017 15.9307 13.0186 15.7966 14M13.6792 20.8436C14.2909 19.6226 14.7924 18.3369 15.1707 17M19.0097 18.132C19.6547 15.8657 20 13.4732 20 11C20 6.58172 16.4183 3 12 3C10.5429 3 9.17669 3.38958 8 4.07026M3 15.3641C3.64066 14.0454 4 12.5646 4 11C4 9.54285 4.38958 8.17669 5.07026 7M11.9999 11C11.9999 14.5172 10.9911 17.7988 9.24707 20.5712" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg><span id="login-biometric-label">{e.bioBtnLabel}</span>
            </button>
        {/if}
    </div>
{/if}

{#if l.bunker}
    <div id="login-bunker" class="login-bunker-container">
        <div class="login-bunker-fields">
            <div class="login-bunker-heading">
                <img src="./icons/key.svg" class="login-bunker-title-icon" alt="">
                <h3 class="login-bunker-title">Connect Remote Signer</h3>
            </div>
            <p class="login-bunker-helper">Scan in your signer app, or copy the link to paste.</p>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="login-bunker-qr-wrap" class:ready={b.qrReady} onclick={() => h.bunker.openQr()}>
                <div id="bunker-qr" class="login-bunker-qr" aria-label="Nostr Connect QR code" use:qr={b.url}></div>
                <div class="login-bunker-qr-loading" id="bunker-qr-loading">Generating connection link…</div>
            </div>
            <button id="bunker-copy-url-btn" class="login-bunker-copy-btn" class:copied={b.copied} type="button" disabled={!b.url || b.busy} onclick={() => h.bunker.copy()}>
                {b.copied ? 'Copied — paste in your signer' : 'Copy connection link'}
            </button>
            <p class="login-bunker-status {line.kind}" id="bunker-status-text">{line.text}</p>
            <div class="login-bunker-divider"><span>or paste a bunker URL</span></div>
            <div class="login-bunker-field login-bunker-field-action">
                <input type="text" class="login-input login-bunker-input" id="bunker-url-input" placeholder="bunker:// from Amber, etc." autocomplete="off" autocapitalize="none" spellcheck="false" disabled={b.busy} bind:value={b.urlInput} />
                <button id="bunker-connect-btn" class="login-bunker-connect-btn" type="button" aria-label="Connect" disabled={b.busy} onclick={() => h.bunker.connect()}>
                    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <path d="M15 3H16.2C17.8802 3 18.7202 3 19.362 3.32698C19.9265 3.6146 20.3854 4.07354 20.673 4.63803C21 5.27976 21 6.11985 21 7.8V16.2C21 17.8802 21 18.7202 20.673 19.362C20.3854 19.9265 19.9265 20.3854 19.362 20.673C18.7202 21 17.8802 21 16.2 21H15M10 7L15 12M15 12L10 17M15 12L3 12" stroke="#000" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            </div>
        </div>
    </div>
{/if}
