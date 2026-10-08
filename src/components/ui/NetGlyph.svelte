<script>
    // The I2P and Clearnet glyphs (Tor's is inlined by js/tor-glyph.js, which fills every
    // svg.tor-glyph, so these carry their own class). A .tor-state-* class on an ancestor drives
    // every part, as it does Tor's.
    let { kind } = $props();
</script>

{#if kind === 'i2p'}
    <!-- Three tunnels around you: each arc's gap sits at its own angle, and each layer turns on its own. -->
    <svg class="net-glyph i2p-glyph" viewBox="0 0 120 120" aria-hidden="true">
        <circle class="i2p-halo" cx="60" cy="60" r="55"></circle>
        <g class="i2p-layer i2p-layer-3"><circle class="i2p-track" cx="60" cy="60" r="48" pathLength="100"></circle><circle class="i2p-arc" cx="60" cy="60" r="48" pathLength="100" transform="rotate(150 60 60)"></circle></g>
        <g class="i2p-layer i2p-layer-2"><circle class="i2p-track" cx="60" cy="60" r="34" pathLength="100"></circle><circle class="i2p-arc" cx="60" cy="60" r="34" pathLength="100" transform="rotate(30 60 60)"></circle></g>
        <g class="i2p-layer i2p-layer-1"><circle class="i2p-track" cx="60" cy="60" r="20" pathLength="100"></circle><circle class="i2p-arc" cx="60" cy="60" r="20" pathLength="100" transform="rotate(-90 60 60)"></circle></g>
        <circle class="i2p-core" cx="60" cy="60" r="6"></circle>
    </svg>
{:else}
    <svg class="net-glyph globe-glyph" viewBox="0 0 120 120" aria-hidden="true">
        <circle class="globe-halo" cx="60" cy="60" r="55"></circle>
        <circle class="globe-line" cx="60" cy="60" r="40"></circle>
        <ellipse class="globe-line" cx="60" cy="60" rx="17" ry="40"></ellipse>
        <path class="globe-line" d="M20 60h80M25.5 40h69M25.5 80h69"></path>
    </svg>
{/if}
