<script>
    // The relay info dialog's activity log, refreshed every second while it is open.
    import { relayLogsState } from '../../lib/network.svelte.js';
    const st = relayLogsState();
</script>

<!-- Unkeyed: the same line can repeat within a second, and a line holds no state of its own. -->
{#each st.logs as log}
    <li><span class="relay-log-time">{new Date(log.timestamp * 1000).toLocaleTimeString()}</span><span class="relay-log-message {log.level}">{log.message}</span></li>
{:else}
    <li class="relay-log-empty">No activity recorded yet</li>
{/each}
