<script>
    // The Nexus scroll body: the featured front page and the catalogue.
    import Featured from './Featured.svelte';
    import AppList from './AppList.svelte';
    let { h } = $props();
</script>

<Featured {h} />
<AppList {h} />
