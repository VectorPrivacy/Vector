<script>
    // The pack details modal's card inside its overlay: the overlay element is the mount
    // (a body-level layer the deep link and the share flow open), so its visibility and
    // backdrop dismiss are set on it from here.
    import { onMount } from 'svelte';
    import { packDetails, closePackDetails } from '../lib/packdetails.svelte.js';
    import PackDetailsModal from './PackDetailsModal.svelte';

    let { overlay, h } = $props();   // h: the modal body's bag
    const d = $derived(packDetails());
    $effect(() => { overlay.hidden = !d; });
    // Backdrop dismiss: only when the click landed on the overlay itself.
    onMount(() => {
        const onClick = (e) => { if (e.target === overlay) closePackDetails(); };
        overlay.addEventListener('click', onClick);
        return () => overlay.removeEventListener('click', onClick);
    });
</script>

<svelte:document onkeydown={(e) => { if (e.key === 'Escape' && d) closePackDetails(); }} />

<div class="pack-details-card" id="pack-details-card">
    <button type="button" class="pack-details-close" id="pack-details-close" aria-label="Close" onclick={closePackDetails}>
        <span class="icon icon-x"></span>
    </button>
    <div class="pack-details-body" id="pack-details-body">
        <PackDetailsModal {h} />
    </div>
</div>
