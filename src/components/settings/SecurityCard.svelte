<script>
    // The Security section's chrome as one reconciler. Renderless: the encryption
    // flows (PIN modals, migration overlay) keep their elements and handlers; this
    // derives the toggle, the two credential rows and the signer card from state.
    import { securityState } from '../lib/settings.svelte.js';

    let { els } = $props();   // toggle, unlockRow, unlockLabel, unlockBtn, pinRow, pinLabel, card, label, hint, pubkey, dot, exportRow

    const s = securityState();
    const isBio = $derived(s.type === 'biometric');
    const credName = $derived(s.type === 'password' ? 'Password' : 'PIN');

    $effect(() => { els.toggle.checked = s.enabled; });
    // Biometric accounts have no typeable credential; the unlock row switches them.
    $effect(() => {
        els.pinRow.style.display = s.enabled && !isBio ? '' : 'none';
        els.pinLabel.textContent = `Change ${credName}`;
    });
    // Hidden with encryption off (nothing to unlock) or when the device cannot do
    // biometrics and is already on a credential (no alternative).
    $effect(() => {
        els.unlockRow.style.display = s.enabled && (s.bioSupported || isBio) ? '' : 'none';
        els.unlockLabel.textContent = isBio ? 'Unlock: Biometrics' : `Unlock: ${credName}`;
        els.unlockBtn.textContent = isBio ? 'Use PIN' : 'Use Biometrics';
    });
    // An external signer keeps the identity key off this device, so Export goes.
    $effect(() => {
        const sg = s.signer;
        els.card.style.display = sg ? '' : 'none';
        els.exportRow.style.display = sg ? 'none' : '';
        if (!sg) return;
        els.label.textContent = sg.label;
        els.hint.textContent = sg.hint;
        els.pubkey.textContent = sg.npub ? `${sg.npub.slice(0, 12)}…${sg.npub.slice(-6)}` : '…';
        els.pubkey.title = sg.npub || '';
    });
    $effect(() => {
        els.dot.classList.remove('online', 'offline', 'connecting');
        if (s.dot) els.dot.classList.add(s.dot);
    });
</script>
