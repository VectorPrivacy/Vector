/**
 * Tor glyph SVG injector.
 *
 * Populates every `<svg class="tor-glyph">` on the page with the same
 * inline content. This is a workaround for `<symbol>` + `<use>`: CSS
 * selectors targeting elements *inside* a use'd shadow tree don't
 * match (animations, opacity, transform from outside selectors won't
 * cross the shadow boundary), so we inline the content directly into
 * each glyph instead — costs a few KB of duplicated DOM but every
 * `.tor-state-*` class on a wrapper now drives every nested element
 * exactly as expected.
 *
 * Single source of truth lives here; updates to the design only need
 * touching one string.
 */
const TOR_GLYPH_SVG = `
  <!-- Halo (only visible in connected state) -->
  <circle class="tor-halo" cx="60" cy="60" r="55"></circle>
  <!-- Three nested rings, dim by default. pathLength=100 so the same
       stroke-dasharray numbers work uniformly across radii. -->
  <circle class="tor-ring tor-ring-3" cx="60" cy="60" r="48" pathLength="100"></circle>
  <circle class="tor-ring tor-ring-2" cx="60" cy="60" r="34" pathLength="100"></circle>
  <circle class="tor-ring tor-ring-1" cx="60" cy="60" r="20" pathLength="100"></circle>
  <!-- Comet trails: only visible while bootstrapping. Each is a
       full-circle stroke with a short dasharray, animated via
       stroke-dashoffset to sweep around. -->
  <circle class="tor-comet tor-comet-3" cx="60" cy="60" r="48" pathLength="100"></circle>
  <circle class="tor-comet tor-comet-2" cx="60" cy="60" r="34" pathLength="100"></circle>
  <circle class="tor-comet tor-comet-1" cx="60" cy="60" r="20" pathLength="100"></circle>
  <!-- Orbital dots: only visible while connected. One dot per ring,
       sitting precisely on its ring's circumference (cy = 60 - r).
       Each dot's wrapping <g> rotates around the glyph center at its
       own speed for the subtle stable orbit. -->
  <g class="tor-dot-orbit tor-dot-orbit-3"><circle class="tor-dot tor-dot-3" cx="60" cy="12" r="3.5"></circle></g>
  <g class="tor-dot-orbit tor-dot-orbit-2"><circle class="tor-dot tor-dot-2" cx="60" cy="26" r="3"></circle></g>
  <g class="tor-dot-orbit tor-dot-orbit-1"><circle class="tor-dot tor-dot-1" cx="60" cy="40" r="2.5"></circle></g>
  <!-- Core (you). Always visible. Pulses while bootstrapping,
       steady while connected. -->
  <circle class="tor-core" cx="60" cy="60" r="6"></circle>
`;

function _injectTorGlyphs() {
    const targets = document.querySelectorAll('svg.tor-glyph');
    targets.forEach((svg) => {
        // Only inject once per element.
        if (svg.dataset.torGlyphInjected) return;
        svg.dataset.torGlyphInjected = '1';
        svg.innerHTML = TOR_GLYPH_SVG;
    });
}

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', _injectTorGlyphs, { once: true });
} else {
    _injectTorGlyphs();
}
