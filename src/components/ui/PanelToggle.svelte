<script>
    // The details column's show/hide control. The shape never changes; the fill marks it
    // open and the label names the direction.
    let { expanded = false, controls, onclick } = $props();
    const NOUN = 'Details';
</script>

<button type="button" class="details-toggle" aria-expanded={expanded ? 'true' : 'false'} aria-controls={controls}
        aria-label="{expanded ? 'Hide' : 'Show'} {NOUN.toLowerCase()}" title="{expanded ? 'Hide' : 'Show'} {NOUN}" {onclick}>
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.84" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="3" y="4.5" width="18" height="15" rx="2"/>
        <path class="fill" d="M15 4.5h4a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-4z" fill="currentColor" stroke="none"/>
        <line x1="15" y1="4.5" x2="15" y2="19.5"/>
    </svg>
</button>
