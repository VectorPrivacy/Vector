<script>
    // One credential prompt: pick PIN or password, enter a PIN (submits on the sixth digit),
    // enter a password, or a validating hold with no controls while the backend checks.
    import { credentialState, credentialHandlers } from '../lib/credential.svelte.js';
    import PinInput from '../ui/PinInput.svelte';
    const c = credentialState();
    const h = () => credentialHandlers();
    const types = [
        { id: 'pin', label: 'PIN', desc: 'A 6-digit code. Quick and convenient.' },
        { id: 'password', label: 'Password', desc: 'A text password. More secure, but slower to enter.' },
    ];
    function submitPassword() { if (c.password) h()?.submit(c.password); }
    function focus(node) { requestAnimationFrame(() => node.focus()); }
</script>

<svelte:window onkeydown={(e) => { if (c.open && c.mode !== 'validating' && e.key === 'Escape') { e.preventDefault(); h()?.cancel(); } }} />

<div class="vcard-overlay cred-overlay" class:active={c.open}>
    {#if c.open}
        <div class="vcard cred-card" role="dialog" aria-modal="true" aria-labelledby="cred-title">
            <div class="vcard-head">
                <span class="vcard-badge cred-badge" aria-hidden="true"></span>
                <h3 id="cred-title" class="vcard-title">{c.title}</h3>
                {#if c.mode !== 'validating'}
                    <button type="button" class="vcard-x" aria-label="Cancel" onclick={() => h()?.cancel()}>&#x2715;</button>
                {/if}
            </div>
            {#if c.mode === 'validating'}
                <div class="xfer-wait" role="status"><span class="xfer-spin" aria-hidden="true"></span>{c.subtitle || 'Checking…'}</div>
            {:else}
                {#if c.subtitle}
                    <p class="vcard-lead" class:cred-error={c.subtitleError} role={c.subtitleError ? 'alert' : undefined}>{c.subtitle}</p>
                {/if}
                {#if c.mode === 'type-select'}
                    <div class="cred-types" role="radiogroup" aria-label="Unlock with">
                        {#each types as t (t.id)}
                            <button type="button" class="cred-type" class:active={c.selectedType === t.id} role="radio"
                                    aria-checked={c.selectedType === t.id} onclick={() => { c.selectedType = t.id; }}>
                                <span class="cred-type-label">{t.label}</span>
                                <span class="cred-type-desc">{t.desc}</span>
                            </button>
                        {/each}
                    </div>
                {:else if c.mode === 'pin'}
                    <PinInput cls="cred-pins" inputClass="cred-pin" resetSeq={c.pinTick} onFull={(pin) => h()?.submit(pin)} />
                {:else if c.mode === 'password'}
                    <input type="password" class="vcard-input" placeholder="Password" autocomplete="off" use:focus
                           bind:value={c.password} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); submitPassword(); } }}>
                {/if}
                <div class="vcard-actions">
                    {#if c.mode === 'type-select'}
                        <button type="button" class="vcard-btn" onclick={() => h()?.submit(c.selectedType)}>{c.confirmText}</button>
                    {:else if c.mode === 'password'}
                        <button type="button" class="vcard-btn" disabled={!c.password} onclick={submitPassword}>{c.confirmText}</button>
                    {/if}
                    <button type="button" class="vcard-btn ghost" onclick={() => h()?.cancel()}>Cancel</button>
                </div>
            {/if}
        </div>
    {/if}
</div>
