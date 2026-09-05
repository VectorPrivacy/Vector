<script>
    // The /command picker: a loading or message state, the sectioned list
    // (recents, then one section per bot), or the armed command's hint row.
    import Popup from './Popup.svelte';
    import { composerPopup } from '../lib/composer.svelte.js';

    let { anchor } = $props();

    const open = $derived(composerPopup().kind === 'command');
    let view = $state.raw({ mode: 'loading', label: '' });
    $effect(() => { if (open) view = composerPopup(); });

    const isMessage = $derived(view.mode === 'loading' || view.mode === 'message');

    // Keyboard nav brings the highlighted row into view (scroll-margin keeps it
    // clear of the stuck section header).
    function keepVisible(node, active) {
        const apply = (a) => { if (a) node.scrollIntoView({ block: 'nearest' }); };
        apply(active);
        return { update: apply };
    }
</script>

<Popup cls="command-selector" {open} {anchor} {view} maxWidth={420} viewportInset message={isMessage}>
    {#if view.mode === 'loading'}
        <div class="command-loading"><span class="command-spinner"></span><span>{view.label}</span></div>
    {:else if view.mode === 'message'}
        {#if view.variant === 'refreshing'}
            <div class="command-loading command-refreshing"><span class="command-spinner"></span><span>{view.label}</span></div>
        {:else}
            <div class="command-empty">{view.label}</div>
        {/if}
    {:else if view.mode === 'list'}
        {#each view.sections as section (section.key)}
            <div class="command-section">
                <div class="command-section-header">
                    {#if section.avatarSrc}<img src={section.avatarSrc} alt="" />{/if}
                    <span>{section.title}</span>
                    {#if section.refreshing}
                        <span class="command-section-refresh"><span class="command-spinner"></span><span>Checking for Updates</span></span>
                    {/if}
                </div>
                {#each section.rows as row (row.key)}
                    <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla row) -->
                    <div
                        class="command-item"
                        class:active={row.index === view.active}
                        title={row.bot ? row.bot.name : undefined}
                        use:keepVisible={row.index === view.active}
                        onmousedown={(e) => { e.preventDefault(); view.pick(row.index); }}
                    >
                        {#if row.bot}
                            <img class="command-item-bot" src={row.bot.avatarSrc || 'icons/user-placeholder.svg'} alt="" />
                        {/if}
                        <span class="command-item-name">/{row.name}</span>
                        {#each row.args as arg}
                            <span class="command-item-arg" class:optional={arg.optional}>{arg.label}</span>
                        {/each}
                        {#if row.description}<span class="command-item-desc">{row.description}</span>{/if}
                    </div>
                {/each}
            </div>
        {/each}
    {:else if view.mode === 'hint'}
        <div class="command-hint">
            <span class="command-item-name">/{view.name}</span>
            {#each view.args as arg}
                <span class="command-item-arg" class:optional={arg.optional} class:current={arg.current} title={arg.title}>{arg.label}</span>
            {/each}
        </div>
        {#if view.desc}<div class="command-hint-desc">{view.desc}</div>{/if}
        {#if view.choices.length}
            <div class="command-hint-choices">
                {#each view.choices as choice}
                    <!-- svelte-ignore a11y_no_static_element_interactions -->
                    <span class="command-choice" onmousedown={(e) => { e.preventDefault(); view.pickChoice(choice); }}>{choice}</span>
                {/each}
            </div>
        {/if}
    {/if}
</Popup>
