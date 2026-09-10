<script>
    // One credential prompt: pick PIN or password, enter a PIN (submits on the sixth digit),
    // enter a password, or a validating hold with no controls while the backend checks.
    import { credentialState, credentialHandlers } from '../lib/credential.svelte.js';
    import PinInput from '../ui/PinInput.svelte';
    const c = credentialState();
    const h = () => credentialHandlers();
    const typeDesc = $derived(c.selectedType === 'pin'
        ? 'A 6-digit code. Quick and convenient.'
        : 'A text password. More secure, but slower to enter.');
    function submitPassword() { if (c.password) h()?.submit(c.password); }
    function focus(node) { requestAnimationFrame(() => node.focus()); }
</script>

<svelte:window onkeydown={(e) => { if (c.open && c.mode !== 'validating' && e.key === 'Escape') { e.preventDefault(); h()?.cancel(); } }} />

{#if c.open}
    <div class="encryption-migration-overlay active">
        <div class="encryption-migration-modal credential-modal">
            <div class="encryption-migration-icon">
                <span class="icon icon-locked"></span>
            </div>
            <h2>{c.title}</h2>
            <p class="encryption-migration-phase" class:startup-subtext-gradient={c.subtitleGradient}>{c.subtitle}</p>
            {#if c.mode === 'type-select'}
                <div>
                    <div class="security-type-selector">
                        <button class="security-type-btn" class:active={c.selectedType === 'pin'} onclick={() => { c.selectedType = 'pin'; }}>PIN</button>
                        <button class="security-type-btn" class:active={c.selectedType === 'password'} onclick={() => { c.selectedType = 'password'; }}>Password</button>
                    </div>
                    <p class="login-encrypt-description">{typeDesc}</p>
                </div>
            {:else if c.mode === 'pin'}
                <PinInput cls="row pin-row" inputClass="cred-pin" resetSeq={c.pinTick} onFull={(pin) => h()?.submit(pin)} />
            {:else if c.mode === 'password'}
                <div id="credential-modal-password">
                    <input type="password" id="credential-modal-password-input" placeholder="Enter your password" autocomplete="off" use:focus
                           bind:value={c.password} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); submitPassword(); } }}>
                </div>
            {/if}
            {#if c.mode !== 'validating'}
                <div id="credential-modal-buttons" class="credential-modal-buttons">
                    <button class="btn cancel-btn" onclick={() => h()?.cancel()}>Cancel</button>
                    {#if c.mode === 'type-select'}
                        <button class="btn" onclick={() => h()?.submit(c.selectedType)}>{c.confirmText}</button>
                    {:else if c.mode === 'password'}
                        <button class="btn" onclick={submitPassword}>{c.confirmText}</button>
                    {/if}
                </div>
            {/if}
        </div>
    </div>
{/if}
