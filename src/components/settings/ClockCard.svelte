<script>
    // The Clock section: which clock Vector stamps and judges by, and what the relays said.
    import InfoIcon from './InfoIcon.svelte';
    import { clock } from '../lib/clock.svelte.js';

    /** @type {{ h: import('../../js/settings.js').ClockHelpers }} */
    let { h } = $props();

    function span(ms) {
        const s = Math.round(Math.abs(ms) / 1000);
        if (s < 90) return `${s} second${s === 1 ? '' : 's'}`;
        if (s < 5400) { const m = Math.round(s / 60); return `${m} minute${m === 1 ? '' : 's'}`; }
        const hrs = Math.round(s / 3600);
        return `${hrs} hour${hrs === 1 ? '' : 's'}`;
    }

    const apart = $derived(clock.measuredOffsetMs > 0 ? `${span(clock.measuredOffsetMs)} slow` : `${span(clock.measuredOffsetMs)} fast`);
    const note = $derived.by(() => {
        if (!clock.checkedAt) return 'Checking the time with your relays…';
        if (clock.agreeing < 2) return 'Your relays did not agree on the time, so this device’s clock is in use.';
        if (clock.measuredOffsetMs === 0) return 'This device’s clock matches your relays.';
        return clock.decentralised
            ? `This device’s clock is ${apart}. Vector is using your relays’ time.`
            : `Your relays say this device’s clock is ${apart}.`;
    });
</script>

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('clock')} />Decentralised Clock</span>
        <input type="checkbox" checked={clock.decentralised}
               onchange={async (e) => { const box = e.currentTarget; await h.setDecentralised(box.checked); box.checked = clock.decentralised; }}>
        <span class="neon-toggle"></span>
    </label>
    <span class="notif-note">{note}</span>
</div>
