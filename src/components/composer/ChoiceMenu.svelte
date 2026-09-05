<script>
    // The picker parts' drop-up: options anchored above their trigger, clamped by
    // the menu's real width (long labels grow it past the trigger).
    import { choiceMenu } from '../lib/composer.svelte.js';

    const menu = $derived(choiceMenu());
    let el;
    // The last view outlives the close, like the popups.
    let view = $state.raw({ options: [], active: 0, pick: () => {} });
    $effect(() => { if (menu.open) view = menu; });

    $effect(() => {
        if (!menu.open) return;
        view;
        const r = menu.anchor.getBoundingClientRect();
        const margin = 8;
        el.style.minWidth = Math.max(Math.ceil(r.width), 110) + 'px';
        el.style.bottom = (window.innerHeight - r.top + 4) + 'px';
        el.style.left = Math.max(margin, Math.min(r.left, window.innerWidth - el.offsetWidth - margin)) + 'px';
        el.querySelector('.command-choice-option.active')?.scrollIntoView({ block: 'nearest' });
    });
</script>

<div bind:this={el} class="command-choice-menu" class:visible={menu.open}>
    {#each view.options as opt, i (opt.v)}
        <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla row) -->
        <div
            class="command-choice-option"
            class:active={i === view.active}
            class:skip={opt.v === ''}
            title={opt.label}
            onmousedown={(e) => { e.preventDefault(); view.pick(opt); }}
        >
            {#if opt.avatarSrc}<img src={opt.avatarSrc} alt="" />{/if}
            <span>{opt.label}</span>
        </div>
    {/each}
</div>
