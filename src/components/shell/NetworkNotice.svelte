<script>
    // One line over the chat list while the account's network is holding traffic back, saying
    // why in a few words (the full reason is the tooltip); it opens Routing. Starting up is
    // normal and says nothing here, until it stalls.
    import { transportState, startStuck } from '../lib/transport.svelte.js';
    import { shellHandlers } from '../lib/shell.svelte.js';
    import { loginUp } from '../lib/login.svelte.js';

    // The pane is narrow: the reason's gist, by its code.
    const SHORT = {
        router_unreachable: 'I2P router not answering.',
        sam_refused: "That port isn't an I2P router.",
        sam_too_old: 'I2P router too old for Vector.',
        sam_auth_required: 'I2P router asks for a password.',
        sam_auth_failed: 'I2P router refused the password.',
        session_failed: "I2P router couldn't open a session.",
        session_lost: 'Reconnecting to I2P.',
        tor_failed: "Tor couldn't connect.",
    };

    // Amber for what mends itself or still half works; red when the user has to act.
    const MENDING = new Set(['session_lost']);

    const t = transportState();
    // A start is timed from its phase's beginning: the clock ticks only while one runs.
    let now = $state(Date.now());
    $effect(() => {
        if (t.view?.phase !== 'starting') return;
        now = Date.now();
        const timer = setInterval(() => { now = Date.now(); }, 5000);
        return () => clearInterval(timer);
    });
    const notice = $derived.by(() => {
        const v = t.view;
        if (!v || v.kind === 'clearnet' || loginUp()) return null;
        if (v.kind === 'unknown') return line(v.reason);
        if (!v.ready && v.phase === 'starting') {
            if (!startStuck(v, now)) return null;
            const text = `${v.label || 'The network'} isn't starting.`;
            return { text, title: text, tone: 'warn' };
        }
        if (!v.ready) return line(v.reason);
        if (t.stranded) return { text: 'None of your relays are on I2P.', title: 'I2P-Only is on and none of your relays are on I2P.', tone: 'bad' };
        // Ready, but every outproxy is down: only I2P servers answer.
        const text = 'No outproxy is reachable right now.';
        return (v.steps || []).some((s) => s.id === 'exit' && s.state === 'fail') ? { text, title: text, tone: 'warn' } : null;
    });

    function line(reason) {
        if (!reason?.text) return null;
        return { text: SHORT[reason.code] || reason.text, title: reason.text, tone: MENDING.has(reason.code) ? 'warn' : 'bad' };
    }
</script>

{#if notice}
    <button type="button" class="net-notice {notice.tone}" title={notice.title} onclick={() => shellHandlers().openRouting?.()}>
        <span class="net-notice-dot" aria-hidden="true"></span>
        <span class="net-notice-text">{notice.text}</span>
        <svg class="net-notice-go" viewBox="0 0 8 12" aria-hidden="true"><path d="M1.5 1.5 6 6l-4.5 4.5"/></svg>
    </button>
{/if}
