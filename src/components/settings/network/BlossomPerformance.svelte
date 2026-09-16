<script>
    // This device's history with one media server: latency (comparable between
    // servers), upload speed (this uplink's, so shown, never graded), and the last few
    // uploads as a curve — with the running average drawn through it and each upload's
    // speed and size a hover away.
    let { stats, h } = $props();
    const latency = $derived(stats.latency_ms != null ? Math.round(stats.latency_ms) : null);
    function mbit(v) {
        if (v == null) return null;
        return v >= 10 ? v.toFixed(0) : v.toFixed(1);
    }
    // A sample is [mbps, bytes]; rows from before sizes were kept are a bare number.
    const samples = $derived((stats.recent || []).map(s => Array.isArray(s) ? { mbps: s[0], bytes: s[1] } : { mbps: s, bytes: 0 }));

    const W = 300, H = 44, PAD = 4;
    const scale = $derived(Math.max(...samples.map(s => s.mbps), stats.mbps || 0, 0.001));
    const yOf = (v) => H - PAD - (v / scale) * (H - PAD * 2);
    const points = $derived.by(() => {
        if (samples.length < 2) return [];
        return samples.map((s, i) => ({
            x: PAD + (i / (samples.length - 1)) * (W - PAD * 2),
            y: yOf(s.mbps),
            ...s,
        }));
    });
    // Catmull-Rom through every point, as cubic Béziers: the line passes through each
    // upload exactly and bends between them instead of cornering.
    const curve = $derived.by(() => {
        if (!points.length) return '';
        const p = points;
        let d = `M${p[0].x.toFixed(1)},${p[0].y.toFixed(1)}`;
        for (let i = 0; i < p.length - 1; i++) {
            const p0 = p[i - 1] || p[i], p1 = p[i], p2 = p[i + 1], p3 = p[i + 2] || p2;
            const c1x = p1.x + (p2.x - p0.x) / 6, c1y = p1.y + (p2.y - p0.y) / 6;
            const c2x = p2.x - (p3.x - p1.x) / 6, c2y = p2.y - (p3.y - p1.y) / 6;
            d += ` C${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ${p2.x.toFixed(1)},${p2.y.toFixed(1)}`;
        }
        return d;
    });
    const area = $derived(curve ? `${curve} L${points[points.length - 1].x.toFixed(1)},${H} L${points[0].x.toFixed(1)},${H} Z` : '');
    const avgY = $derived(stats.mbps != null && points.length ? yOf(stats.mbps) : null);

    // The nearest upload to the pointer, shown while hovering or after a tap.
    let hover = $state(null);
    function pick(e) {
        if (!points.length) return;
        const r = e.currentTarget.getBoundingClientRect();
        const x = ((e.clientX - r.left) / r.width) * W;
        let best = points[0];
        for (const p of points) if (Math.abs(p.x - x) < Math.abs(best.x - x)) best = p;
        hover = best;
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
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="blossom-perf-chart" onpointermove={pick} onpointerdown={pick} onpointerleave={() => hover = null}>
                <!-- Stretched to the box; strokes don't scale, so the line stays thin and a
                     zero-length round-capped stroke stays a round dot. -->
                <svg class="blossom-perf-spark" viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-label="Speed of recent uploads, oldest to newest">
                    <defs>
                        <linearGradient id="blossom-perf-fill" x1="0" y1="0" x2="0" y2="1">
                            <stop offset="0" stop-color="var(--accent-color, #59fcb3)" stop-opacity="0.22" />
                            <stop offset="1" stop-color="var(--accent-color, #59fcb3)" stop-opacity="0" />
                        </linearGradient>
                    </defs>
                    <path d={area} fill="url(#blossom-perf-fill)" />
                    {#if avgY != null}
                        <line x1="0" y1={avgY.toFixed(1)} x2={W} y2={avgY.toFixed(1)} stroke="currentColor" stroke-width="1" stroke-dasharray="3 4" opacity="0.28" vector-effect="non-scaling-stroke" />
                    {/if}
                    <path d={curve} fill="none" stroke="var(--accent-color, #59fcb3)" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" opacity="0.75" vector-effect="non-scaling-stroke" />
                    {#each points as p, i}
                        <path d="M{p.x.toFixed(1)},{p.y.toFixed(1)}h0.01" stroke="var(--accent-color, #59fcb3)" stroke-width={hover === p ? 7 : (i === points.length - 1 ? 6 : 4)} stroke-linecap="round" vector-effect="non-scaling-stroke" />
                    {/each}
                </svg>
                {#if hover}
                    <div class="blossom-perf-tip" style="left: {(hover.x / W * 100).toFixed(1)}%; top: {(hover.y / H * 100).toFixed(1)}%">
                        <b>{mbit(hover.mbps)} Mbit/s</b>{#if hover.bytes}{' · '}{h.formatBytes(hover.bytes, 1)}{/if}
                    </div>
                {/if}
            </div>
        {/if}
        <div class="blossom-perf-foot">
            <span>
                {#if points.length}Speed of your last {samples.length} uploads{:else}{stats.uploads} upload{stats.uploads === 1 ? '' : 's'}{/if}{#if stats.bytes_total}{' · '}{h.formatBytes(stats.bytes_total, 1)}{/if}
            </span>
            {#if avgY != null}<span>dashed: average</span>{/if}
        </div>
    </div>
{/if}
