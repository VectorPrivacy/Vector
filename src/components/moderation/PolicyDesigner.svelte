<script>
    // The Policies tab: the community's policies and the template gallery, or the editor
    // for one draft. A policy is never enabled from a form alone, only from a preview.
    import { polState } from '../lib/policy.svelte.js';
    import PolicyList from './PolicyList.svelte';
    import PolicyGallery from './PolicyGallery.svelte';
    import PolicyEditor from './PolicyEditor.svelte';
    let { h } = $props();
    const st = polState();
</script>

{#if st.view === 'editor'}
    <PolicyEditor {h} />
{:else}
    <PolicyList {h} />
    <PolicyGallery {h} />
{/if}
