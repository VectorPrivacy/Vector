<script>
    // The :shortcode autocomplete. Custom emojis go through the cached-image
    // pipeline (never a raw remote src); stock ones render their twemoji glyph.
    import AnchoredPanel from './AnchoredPanel.svelte';
    import { composerPopup } from '../lib/composer.svelte.js';

    let { anchor, h } = $props();   // h: bindCachedEmojiImg(img, url, kind), twemojiUrl(emoji)

    const open = $derived(composerPopup().kind === 'shortcode');
    let view = $state.raw({ header: '', items: [], active: 0, pick: () => {} });
    $effect(() => { if (open) view = composerPopup(); });

    const key = (item) => (item.isCustom ? ':' + item.shortcode + ':' : item.emoji);
    const label = (item) => (item.shortcode ? ':' + item.shortcode + ':' : item.name.split(' ').slice(0, 3).join(' '));

    function cached(img, url) {
        h.bindCachedEmojiImg(img, url, 'emoji');
    }
</script>

<AnchoredPanel cls="emoji-shortcode-selector" {open} {anchor} {view}>
    <div class="emoji-shortcode-header">{view.header}</div>
    {#each view.items as item, i (key(item))}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
            class="emoji-shortcode-item"
            class:active={i === view.active}
            onmousedown={(e) => { e.preventDefault(); view.pick(i); }}
        >
            {#if item.isCustom}
                <img class="emoji-shortcode-item-custom" alt=":{item.shortcode}:" use:cached={item.url} />
            {:else if h.twemojiUrl(item.emoji)}
                <img src={h.twemojiUrl(item.emoji)} alt={item.emoji} />
            {:else}
                <span class="emoji-shortcode-item-fallback">{item.emoji}</span>
            {/if}
            <span class="emoji-shortcode-item-label">{label(item)}</span>
        </div>
    {/each}
</AnchoredPanel>
