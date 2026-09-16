<script>
    // This device's history with one media server: latency (comparable between
    // servers), upload speed (this uplink's, so shown, never graded), and a sparkline
    // of the last few uploads.
    let { stats, h } = $props();
    const latency = $derived(stats.latency_ms != null ? Math.round(stats.latency_ms) : null);
    function mbit(v) {
        if (v == null) return null;
        return v >= 10 ? v.toFixed(0) : v.toFixed(1);
    }
    const recent = $derived(stats.recent || []);
    // One point per recent upload, oldest left, scaled to the fastest of them.
    const points = $derived.by(() => {
        if (recent.length < 2) return [];
        const w = 300, hgt = 34, pad = 4;
        const max = Math.max(...recent, 0.001);
        return recent.map((v, i) => ({
            x: pad + (i / (recent.length - 1)) * (w - pad * 2),
            y: hgt - pad - (v / max) * (hgt - pad * 2),
        }));
    });
    const spark = $derived(points.map(p => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(' '));
    function ago(ts) {
        if (!ts) return '';
        const s = Math.max(0, Math.floor(Date.now() / 1000) - ts);
        if (s < 60) return 'just now';
        if (s < 3600) return `${Math.floor(s / 60)} min ago`;
        if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
        return `${Math.floor(s / 86400)} d ago`;
    }
</script>

{#if latency != null || stats.mbps != null}
    <div class="relay-metrics-section">
        <div class="blossom-perf">
            <div class="blossom-perf-stat">
                <span class="blossom-perf-label">Latency</span>
                <span class="blossom-perf-value">{#if latency != null}{latency} <span class="blossom-perf-unit">ms</span>{:else}–{/if}</span>
            </div>
            <div class="blossom-perf-stat">
                <span class="blossom-perf-label">Upload</span>
                <span class="blossom-perf-value">{#if stats.mbps != null}{mbit(stats.mbps)} <span class="blossom-perf-unit">Mbit/s</span>{:else}–{/if}</span>
            </div>
            <div class="blossom-perf-stat">
                <span class="blossom-perf-label">Best</span>
                <span class="blossom-perf-value">{#if stats.best_mbps != null}{mbit(stats.best_mbps)} <span class="blossom-perf-unit">Mbit/s</span>{:else}–{/if}</span>
            </div>
        </div>
        {#if points.length}
            <!-- Stretched to the box; strokes don't scale, so the line stays thin and a
                 zero-length round-capped stroke stays a round dot. -->
            <svg class="blossom-perf-spark" viewBox="0 0 300 34" preserveAspectRatio="none" aria-label="Speed of recent uploads, oldest to newest">
                <polyline points={spark} fill="none" stroke="var(--accent-color, #59fcb3)" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" opacity="0.6" vector-effect="non-scaling-stroke" />
                {#each points as p}
                    <path d="M{p.x.toFixed(1)},{p.y.toFixed(1)}h0.01" stroke="var(--accent-color, #59fcb3)" stroke-width="5" stroke-linecap="round" vector-effect="non-scaling-stroke" />
                {/each}
            </svg>
        {/if}
        <div class="blossom-perf-foot">
            <span>
                {#if points.length}Speed of your last {recent.length} uploads{:else}{stats.uploads} upload{stats.uploads === 1 ? '' : 's'}{/if}{#if stats.bytes_total}{' · '}{h.formatBytes(stats.bytes_total, 1)}{/if}
            </span>
            {#if stats.last_ok_at}<span>last seen {ago(stats.last_ok_at)}</span>{/if}
        </div>
    </div>
{/if}
