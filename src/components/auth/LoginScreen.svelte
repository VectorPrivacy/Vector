<script>
    // The login screen inside #login-form: the lockup and pre-login account picker on top,
    // one centred step at a time (start / import / bunker / invite / welcome / encrypt),
    // the credits and links along the bottom, and the illustration behind it all. Painted
    // from lib/login.svelte.js; every control answers through `h` (js/auth.js).
    import { untrack } from 'svelte';
    import { loginState, bunkerState, pickerState, encryptState, toggleLoginBg } from '../lib/login.svelte.js';
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

    // ── the way back, wherever a step has one ──
    const back = $derived(l.bunker && b.fromImport ? { prompt: 'Have a private key?', label: 'Use nsec instead' }
        : b.mode === 'reauth' && l.bunker || l.screen === 'start' ? { prompt: 'Changed your mind?', label: 'Go Back' }
        : { prompt: 'Want to change accounts?', label: 'Back to Login' });

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
    let reveal = $state(false);
    $effect(() => { e.pinTick; untrack(() => pinRow?.clear(e.pinFocus)); });
    $effect(() => {
        e.focusTick;
        untrack(() => {
            if (e.pinShown) pinRow?.focusFirst();
            else if (e.passwordShown) passwordEl?.focus();
        });
    });
    // iOS opens the keyboard only for a focus made inside a tap, so on the PIN and
    // password steps a tap anywhere reaches the field.
    function tapToType(ev) {
        if (l.screen !== 'encrypt' || ev.target.closest('button, input, a, select, textarea')) return;
        if (e.pinShown) pinRow?.focusNext();
        else if (e.passwordShown) passwordEl?.focus();
    }

    // A fresh password step starts masked.
    $effect(() => { if (!e.passwordShown) reveal = false; });
    function submitPassword(ev) { ev.preventDefault(); h.encrypt.submitPassword(); }
    const onEnter = (fn) => (ev) => { if (ev.key === 'Enter' || ev.code === 'NumpadEnter') { ev.preventDefault(); fn(); } };
</script>

<svelte:window onkeydown={(ev) => { if (b.qrOpen && ev.key === 'Escape') { ev.preventDefault(); h.bunker.closeQr(); } }} />

{#snippet goBack()}
    {#if l.backBar}
        <p class="lg-back">{back.prompt} <button type="button" onclick={() => h.back()}>{back.label}</button></p>
    {/if}
{/snippet}

{#snippet enterIcon()}<img src="./icons/login/enter.svg" alt="" width="22" height="22">{/snippet}

<div class="lg-art" class:hidden={l.bgHidden} aria-hidden="true">
    <img src="./icons/login/cctv.png" alt="">
</div>
<div class="lg-aggro-bg" class:shown={l.screen === 'encrypt' && !!e.wrong} aria-hidden="true">
    <img class="lg-aggro-bg-1" src="./icons/login/aggro-bg-1.svg" alt="" width="226" height="224">
    <img class="lg-aggro-bg-2" src="./icons/login/aggro-bg-2.svg" alt="" width="128" height="127">
    <img class="lg-aggro-bg-3" src="./icons/login/aggro-bg-3.svg" alt="" width="98" height="97">
    <img class="lg-aggro-bg-4" src="./icons/login/aggro-bg-4.svg" alt="" width="60" height="59">
</div>

<div class="lg">
    <header class="lg-head">
        <img class="lg-lockup" src="./icons/login/lockup.svg" alt="Vector">
        {#if p.shown}
            <button type="button" id="login-account-picker" class="lg-account" class:open={p.open} bind:this={pill} onclick={() => h.picker.toggle()}>
                <Avatar src={p.avatar} size={44} />
                <span class="lg-account-name">{p.label}</span>
                <img class="lg-account-chevron" src="./icons/login/chevron.svg" alt="" width="10" height="6">
            </button>
        {/if}
    </header>

    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <main class="lg-stage" onclick={tapToType}>
        {#if l.screen === 'start'}
            <div id="login-start" class="lg-block">
                <div class="lg-buttons">
                    <button type="button" class="lg-btn primary" onclick={() => h.createAccount()}>Create Account</button>
                    <button type="button" class="lg-btn accent" onclick={() => h.openImport()}>Login</button>
                </div>
                {@render goBack()}
            </div>
        {:else if l.screen === 'import'}
            <div class="lg-block">
                <div class="lg-title"><img src="./icons/login/lock.svg" alt="" width="22" height="29"><h3>Enter Your Private Key</h3></div>
                <div class="lg-field-row lg-field-row-offset">
                    <div class="lg-field">
                        <input type="password" class="lg-input" id="login-input" placeholder="Enter nsec or Seed Phrase..."
                               autocomplete="off" spellcheck="false" bind:value={l.importKey} onkeydown={onEnter(() => h.importKey())}>
                        <button type="button" class="lg-field-go" aria-label="Login" onclick={() => h.importKey()}>{@render enterIcon()}</button>
                    </div>
                    <button type="button" class="lg-side-btn" title="Use a Remote Signer" aria-label="Use a Remote Signer"
                            disabled={b.busy} onclick={() => h.bunker.open()}>
                        <img src="./icons/login/swap.svg" alt="" width="18" height="20">
                    </button>
                </div>
                {@render goBack()}
                {#if l.nip55Shown}
                    <button type="button" class="lg-link" disabled={l.nip55Busy} onclick={() => h.nip55()}>Sign in with Amber (Offline)</button>
                {/if}
                {#if l.nip07Shown}
                    <button type="button" class="lg-link" disabled={l.nip07Busy} onclick={() => h.nip07()}>Sign in with Browser Extension</button>
                {/if}
            </div>
        {:else if l.screen === 'invite'}
            <div class="lg-block">
                <div class="lg-title"><h3>Enter Invite Code</h3></div>
                <p class="lg-note">Enter your invite code to join Vector Beta</p>
                <div class="lg-field-row">
                    <div class="lg-field">
                        <!-- svelte-ignore a11y_autofocus -->
                        <input type="text" class="lg-input" placeholder="Invite code..." autofocus bind:value={l.inviteCode} onkeydown={onEnter(() => h.invite())}>
                        <button type="button" class="lg-field-go" aria-label="Submit invite code" onclick={() => h.invite()}>{@render enterIcon()}</button>
                    </div>
                </div>
            </div>
        {:else if l.screen === 'welcome'}
            <div class="lg-block lg-welcome">
                <span class="icon icon-vector-check lg-welcome-icon"></span>
                <p class="lg-welcome-kicker">Welcome to</p>
                <h1 class="lg-welcome-title">Vector Beta!</h1>
                <p class="lg-note">Congratulations! You're now part of the exclusive Vector Beta community.</p>
                <p class="lg-note">Setting up your account...</p>
            </div>
        {:else if l.screen === 'encrypt'}
            <div id="login-encrypt" class="lg-block">
                {#if e.wrong}
                    <div class="lg-aggro" role="alert">
                        <div class="lg-aggro-art">
                            <img class="lg-aggro-face" src="./icons/login/aggroboi.svg" alt="" width="136" height="136">
                            <img class="lg-aggro-warn" src="./icons/login/warning.svg" alt="" width="79" height="70">
                        </div>
                        <p class="lg-aggro-oops">Oops! You entered the wrong {e.wrong}!</p>
                        <h3 class="lg-aggro-title">Enter the Correct {e.wrong === 'pin' ? 'Pin' : 'Password'}.</h3>
                    </div>
                {:else if e.headerShown}
                    <div class="lg-title" class:error={e.error}>
                        {#if e.lockShown}<img src="./icons/login/lock.svg" alt="" width="22" height="29">{/if}
                        <h3 id="login-encrypt-title" class:startup-subtext-gradient={e.gradient} class:typing-indicator-text={e.typing}>{e.title}</h3>
                    </div>
                {/if}
                {#if e.typeSelectShown}
                    <div class="lg-buttons">
                        <button type="button" class="lg-btn" onclick={() => h.encrypt.choose('skip')}>Skip Encryption</button>
                        <button type="button" class="lg-btn" onclick={() => h.encrypt.choose('password')}>Create Password</button>
                        <p class="lg-recommended">{e.recommended}</p>
                        <button type="button" class="lg-btn primary" onclick={() => h.encrypt.choose('pin')}>Create PIN</button>
                        {#if e.bioOptionShown}
                            <button type="button" class="lg-btn primary" onclick={() => h.encrypt.choose('biometric')}>{e.bioOptionLabel}</button>
                        {/if}
                    </div>
                {/if}
                {#if e.pinShown}
                    <PinInput bind:this={pinRow} id="login-encrypt-pins" cls="lg-pins" inputIds={['pin-0', 'pin-1', 'pin-2', 'pin-3', 'pin-4', 'pin-5']}
                              onFull={(pin) => h.encrypt.pinFull(pin)} onBackspace={() => h.encrypt.pinBackspace()} />
                {/if}
                {#if e.passwordShown}
                    <div id="login-encrypt-password" class="lg-field-row">
                        <div class="lg-field">
                            <!-- svelte-ignore a11y_autofocus -->
                            <input type={reveal ? 'text' : 'password'} class="lg-input" class:masked={!reveal} placeholder="Enter Password..." autocomplete="off" autofocus
                                   bind:this={passwordEl} bind:value={e.password} onkeydown={(ev) => { if (ev.key === 'Enter') submitPassword(ev); }}>
                            <button type="button" class="lg-field-eye" aria-label={reveal ? 'Hide password' : 'Show password'} onclick={() => { reveal = !reveal; passwordEl?.focus(); }}>
                                {#if reveal}<span class="icon icon-eye-off"></span>{:else}<img src="./icons/login/eye.svg" alt="" width="22" height="16">{/if}
                            </button>
                            <button type="button" class="lg-field-go" aria-label="Continue" onclick={submitPassword}>{@render enterIcon()}</button>
                        </div>
                    </div>
                {/if}
                {#if e.bioBtnShown}
                    <button type="button" class="lg-btn lg-bio" onclick={() => h.encrypt.biometric()}>
                        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                            <path d="M5.80688 18.5304C5.82459 18.5005 5.84273 18.4709 5.8613 18.4413C7.2158 16.2881 7.99991 13.7418 7.99991 11C7.99991 8.79086 9.79077 7 11.9999 7C14.209 7 15.9999 8.79086 15.9999 11C15.9999 12.017 15.9307 13.0186 15.7966 14M13.6792 20.8436C14.2909 19.6226 14.7924 18.3369 15.1707 17M19.0097 18.132C19.6547 15.8657 20 13.4732 20 11C20 6.58172 16.4183 3 12 3C10.5429 3 9.17669 3.38958 8 4.07026M3 15.3641C3.64066 14.0454 4 12.5646 4 11C4 9.54285 4.38958 8.17669 5.07026 7M11.9999 11C11.9999 14.5172 10.9911 17.7988 9.24707 20.5712" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                        </svg>
                        <span>{e.bioBtnLabel}</span>
                    </button>
                {/if}
                {#if !e.typeSelectShown && (e.pinShown || e.passwordShown)}{@render goBack()}{/if}
            </div>
        {:else if l.screen === 'none'}
            <div class="lg-booting" role="status" aria-label="Loading"><span></span><span></span><span></span></div>
        {/if}

        {#if l.bunker}
            <div class="lg-block">
                <div class="lg-title">
                    <img src="./icons/login/lock.svg" alt="" width="22" height="29">
                    <h3>{b.mode === 'reauth' ? 'Reconnect Your Remote Signer' : 'Login with Remote Signer'}</h3>
                </div>
                <div class="lg-field-row">
                    <div class="lg-field">
                        <input type="text" class="lg-input" placeholder="bunker:// from Amber, etc." autocomplete="off" autocapitalize="none" spellcheck="false"
                               disabled={b.busy} bind:value={b.urlInput} onkeydown={onEnter(() => h.bunker.connect())}>
                        <button type="button" class="lg-field-go" aria-label="Connect" disabled={b.busy} onclick={() => h.bunker.connect()}>{@render enterIcon()}</button>
                    </div>
                    <button type="button" class="lg-side-btn bare" title="Pair with a QR code" aria-label="Pair with a QR code" onclick={() => h.bunker.openQr()}>
                        <img src="./icons/login/qr.svg" alt="" width="40" height="40">
                    </button>
                </div>
                {#if !b.qrOpen && line.text}<p class="lg-status {line.kind}">{line.text}</p>{/if}
                {@render goBack()}
            </div>
        {/if}
    </main>

    <!-- In the steps' own layer, so the pill can rise above this backdrop. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="login-account-list-backdrop" class="login-account-list-backdrop" class:visible={p.open} onclick={() => h.picker.close()}></div>
    <div id="login-account-list" class="login-account-list" class:open={p.open} role="dialog" aria-label="Pick account" bind:this={list}>
        {#if p.open}
            <!-- The active account stays in the pill; only the alternates are rows. -->
            <AccountRows accounts={p.accounts.filter(m => m.npub !== p.activeNpub)} h={h.picker.rowHelpers()} onPick={(m) => h.picker.pick(m)} />
        {/if}
    </div>

    <footer class="lg-foot">
        <div class="lg-credit" aria-label="Developed by Formless Labs">
            <img class="lg-credit-text" src="./icons/login/credit-text.svg" alt="" width="174" height="10">
            <img class="lg-credit-mark" src="./icons/login/credit-mark.svg" alt="" width="13" height="20">
        </div>
        <p class="lg-links">
            <button type="button" onclick={() => h.openLink('website')}>Website</button><span>|</span>
            <button type="button" onclick={() => h.openLink('gitbook')}>Documentation</button><span>|</span>
            <button type="button" onclick={() => h.openLink('privacy')}>Privacy Policy</button><span>|</span>
            <button type="button" onclick={() => h.openLink('donate')}>Donate</button>
        </p>
    </footer>

    <button type="button" class="lg-bg-toggle" onclick={toggleLoginBg}>
        <span class="lg-bg-icon">
            {#if l.bgHidden}<img src="./icons/login/eye.svg" alt="" width="17" height="12">{:else}<img src="./icons/login/hide.svg" alt="" width="17" height="16">{/if}
        </span>
        <span>{l.bgHidden ? 'Show Background' : 'Hide Background'}</span>
    </button>
</div>


{#if b.qrOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="lg-qr-overlay" onclick={(ev) => { if (ev.target === ev.currentTarget) h.bunker.closeQr(); }}>
        <div class="lg-qr-card" role="dialog" aria-label="Pair with a QR code">
            <div class="lg-qr" class:ready={b.qrReady}>
                <div class="lg-qr-code" aria-label="Nostr Connect QR code" use:qr={b.url}></div>
                <p class="lg-qr-loading">{line.kind === 'error' ? line.text : 'Generating connection link…'}</p>
            </div>
            <p class="lg-qr-status">{b.qrReady ? line.text : ''}</p>
            <div class="lg-qr-actions">
                <button type="button" class="lg-qr-copy" class:copied={b.copied} disabled={!b.url || b.busy} onclick={() => h.bunker.copy()}>{b.copied ? 'Copied' : 'Copy Link'}</button>
                <button type="button" class="lg-qr-close" onclick={() => h.bunker.closeQr()}>Close</button>
            </div>
        </div>
    </div>
{/if}
