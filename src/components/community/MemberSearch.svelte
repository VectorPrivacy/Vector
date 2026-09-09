<script>
    // The roster filter box. Widescreen lifts it into the details column's header (the
    // community is named one column left, so the header would otherwise be empty); narrow,
    // it sits above the roster. Both render from the same store text.
    import { overviewState, overviewRoster } from '../lib/overview.svelte.js';
    let { docked = false } = $props();
    const ov = overviewState();
    function filter(e) { ov.memberSearch = e.currentTarget.value; overviewRoster()?.setFilter(ov.memberSearch); }
</script>

<div class="emoji-search-container" class:ws-member-search={docked} style="padding: 10px 0px; background-color: transparent;" style:display={ov.memberSearchShown ? null : 'none'}>
    <span class="emoji-search-icon icon icon-search"></span>
    <input id="group-member-search-input" placeholder="Search Members..." autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
           style="padding: 10px 40px; background-color: transparent; border: 1px solid rgba(57, 57, 57, 0.5);" value={ov.memberSearch} oninput={filter}>
</div>
