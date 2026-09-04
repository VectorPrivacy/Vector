<script>
    // A collapsible rank section of a roster. Collapse is flipped in place (never through a
    // list rebuild, which would throw away scroll position) and remembered per community.
    let {
        label,
        count,
        closed = false,           // remembered state at mount
        forceOpen = false,        // a search opens every section it found someone in
        ontoggle = () => {},      // (closing: boolean)
        children,
    } = $props();

    // svelte-ignore state_referenced_locally
    let isClosed = $state(closed);

    function toggle() {
        isClosed = !isClosed;
        ontoggle(isClosed);
    }
</script>

<div class="member-section" class:is-closed={isClosed && !forceOpen}>
    <div class="member-section-head btn" role="button" tabindex="0" onclick={toggle} onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), toggle())}>
        <span class="member-section-label">{label}</span>
        <span class="member-section-count">{count}</span>
        <span class="member-section-caret"><span class="icon icon-chevron-down"></span></span>
    </div>
    <div class="member-section-body">
        {@render children?.()}
    </div>
</div>
