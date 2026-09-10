<script>
    // The relay info dialog's activity log, refreshed every second while it is open.
    import { relayLogsState } from '../../lib/settings.svelte.js';
    const st = relayLogsState();
</script>

{#each st.logs as log (log.timestamp + log.message)}
    <li><span class="relay-log-time">{new Date(log.timestamp * 1000).toLocaleTimeString()}</span><span class="relay-log-message {log.level}">{log.message}</span></li>
{:else}
    <li class="relay-log-empty">No activity recorded yet</li>
{/each}
