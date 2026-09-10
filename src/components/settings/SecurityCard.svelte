<script>
    // The Security section: the signer card, the encryption toggle, the unlock and
    // credential rows and Export. The flows (PIN modals, migration overlay) stay in
    // the app; a cancelled flip snaps the toggle back when the app re-syncs state.
    import { securityState } from '../lib/settings.svelte.js';
    import InfoIcon from './InfoIcon.svelte';

    let { h } = $props();   // h: toggleEncryption(on), changeCredential(), switchUnlock(), reauthorize(), exportAccount(), help(key)

    const s = securityState();
    const isBio = $derived(s.type === 'biometric');
    const credName = $derived(s.type === 'password' ? 'Password' : 'PIN');
    const short = $derived(s.signer?.npub ? `${s.signer.npub.slice(0, 12)}…${s.signer.npub.slice(-6)}` : '…');

    // The box follows the user's flip until the app confirms or reverts the state.
    let checked = $state(false);
    $effect(() => { s.seq; checked = s.enabled; });
</script>

{#if s.signer}
    <div id="settings-remote-signer" class="remote-signer-card">
        <div class="remote-signer-head">
            <div class="remote-signer-title">
                <span class="remote-signer-dot {s.dot}" id="remote-signer-dot"></span>
                <span id="remote-signer-label">{s.signer.label}</span>
            </div>
            <button id="remote-signer-reauth-btn" class="cancel-btn" onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.reauthorize(); }}>Re-authorize</button>
        </div>
        <div class="remote-signer-meta">
            <div class="remote-signer-meta-row">
                <span class="remote-signer-meta-label">Signer key</span>
                <span class="remote-signer-meta-value remote-signer-mono" id="remote-signer-pubkey" title={s.signer.npub || ''}>{short}</span>
            </div>
        </div>
        <p class="remote-signer-hint" id="remote-signer-hint">{s.signer.hint}</p>
    </div>
{/if}

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('encryption')} />Local Encryption</span>
        <input type="checkbox" id="security-encryption-toggle" bind:checked onchange={() => h.toggleEncryption(checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>

<!-- Hidden with encryption off (nothing to unlock) or when the device cannot do
     biometrics and is already on a credential (no alternative). -->
{#if s.enabled && (s.bioSupported || isBio)}
    <div id="unlock-method-container" class="danger-option">
        <div class="left-group">
            <InfoIcon onclick={() => h.help('unlockMethod')} />
            <span id="unlock-method-label">{isBio ? 'Unlock: Biometrics' : `Unlock: ${credName}`}</span>
        </div>
        <button id="unlock-method-switch" class="btn cancel-btn" onclick={() => h.switchUnlock()}>{isBio ? 'Use PIN' : 'Use Biometrics'}</button>
    </div>
{/if}

<!-- Biometric accounts have no typeable credential; the unlock row switches them. -->
{#if s.enabled && !isBio}
    <div id="change-pin-container" class="danger-option">
        <div class="left-group">
            <InfoIcon onclick={() => h.help('changePin')} />
            <span id="change-pin-label">Change {credName}</span>
        </div>
        <button id="security-change-credential" class="btn cancel-btn" onclick={() => h.changeCredential()}>Change</button>
    </div>
{/if}

<!-- An external signer keeps the identity key off this device, so Export goes. -->
{#if !s.signer}
    <div id="export-account-row" class="danger-option">
        <div class="left-group">
            <InfoIcon onclick={() => h.help('exportAccount')} />
            <span>Export Account</span>
        </div>
        <button id="export-account-btn" class="cancel-btn" onclick={() => h.exportAccount()}>Export</button>
    </div>
{/if}
