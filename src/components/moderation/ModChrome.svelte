<script>
    // Renderless: the console's head, raid banner (or the busy notice that borrows its
    // slot), tallies and the three action buttons, adopted from the markup.
    import { modState, modIntel, modKeep } from '../lib/moderation.svelte.js';
    let { els, h } = $props();   // els: card, name, epoch, alert, alertTitle, alertBody, tallyKeep, tallyCut, banlist, revoke, revokeLabel, rotate, rotateLabel, banRotate, banRotateLabel, close; h: ago(secs)

    const st = modState();
    const intel = $derived(modIntel());
    const keep = $derived(modKeep());
    const cut = $derived(intel ? intel.report.members.filter(x => !keep.has(x.npub)).length : 0);
    const total = $derived(intel ? intel.report.members.length : 0);

    $effect(() => {
        els.name.textContent = intel ? (intel.name || 'Community') : '';
        els.epoch.textContent = intel ? `Epoch ${intel.epoch}` : '';
        els.banlist.textContent = intel ? `banlist ${intel.banlist_count}/${intel.banlist_max}` : '';
        const n = intel ? intel.invites.length : 0;
        els.revokeLabel.textContent = n ? `Revoke ${n} invite${n === 1 ? '' : 's'}` : 'No invite links';
    });
    // The raid banner, or nothing; a publish borrows the slot for its progress.
    $effect(() => {
        els.alert.classList.toggle('working', st.busy);
        if (st.busy) {
            els.alertTitle.textContent = st.busyTitle;
            els.alertBody.textContent = st.busyBody;
            els.alert.style.display = '';
            return;
        }
        const r = intel?.report;
        if (!r || !r.raid_detected) { els.alert.style.display = 'none'; return; }
        // `size` is the true cluster; `members` is only a display sample the backend caps.
        const biggest = r.cohorts[0];
        const burst = r.burst_size >= 2 && r.burst_to_ms > r.burst_from_ms
            ? ` ${r.burst_size} joined within ${h.ago(Math.round((r.burst_to_ms - r.burst_from_ms) / 1000))} of each other.` : '';
        els.alertTitle.textContent = `Raid: ${r.suspects} accounts flagged.`;
        els.alertBody.textContent = (biggest ? ` ${biggest.size} posted “${biggest.sample.slice(0, 40)}”.` : '') + burst
            + ' They start unticked, so they are the ones being removed.';
        els.alert.style.display = '';
    });
    $effect(() => {
        els.tallyKeep.textContent = String(total - cut);
        els.tallyCut.textContent = String(cut);
        // A red "0 removing" reads as a standing alarm; it belongs there once someone is unticked.
        els.tallyCut.parentElement.hidden = cut === 0;
        const room = intel ? intel.banlist_max - intel.banlist_count : 0;
        const overCap = cut > room;
        els.card.classList.toggle('busy', st.busy);
        els.close.disabled = st.busy;
        els.revoke.disabled = st.busy || !intel || intel.invites.length === 0;
        // A bare rotation with nobody cut is legitimate: it answers a leaked link.
        els.rotate.disabled = st.busy || !intel;
        els.banRotate.disabled = st.busy || !intel || cut === 0 || overCap;
        els.banRotate.title = overCap && intel ? `The banlist holds ${intel.banlist_max}; only ${room} slots are free. Rotate instead: it has no ceiling.` : '';
        els.rotateLabel.textContent = cut ? `Remove ${cut} & rotate` : 'Rotate keys';
        els.banRotateLabel.textContent = cut ? `Ban ${cut} & rotate` : 'Ban & rotate';
    });
</script>
