<script>
    // The pack creator's in-panel overlays: confirm, progress, naming, cropper and error.
    // They live inside the picker so their clicks never reach the outside-click close.
    // Each is a store view; the app owns the promise behind confirm, naming and the crop.
    // The cropper stays mounted: its stage, box and preview are the app's pointer leaf.
    import { onMount } from 'svelte';
    import { panelState } from '../lib/picker.svelte.js';

    let { h } = $props();
    // h: bindCachedImg(img, url, kind), errorRetry(), confirm(ok), namingInput(), namingCommit(value), namingCancel(),
    //    cropperEls({ stage, img, box, preview, cancel, ok })

    const p = panelState();

    // A fresh element per open: focus it once it exists.
    function focusLater(el, select) { setTimeout(() => { el.focus(); if (select) el.select(); }, 30); }
    // A blob: URL never touched the network; anything else routes through the cache.
    function preview(img, src) {
        if (typeof src === 'string' && src.startsWith('blob:')) img.src = src;
        else h.bindCachedImg(img, src, 'emoji');
    }

    let stage = $state(null), img = $state(null), box = $state(null), cropPreview = $state(null), cancel = $state(null), ok = $state(null);
    onMount(() => h.cropperEls({ stage, img, box, preview: cropPreview, cancel, ok }));
</script>

{#if p.confirm}
    {@const cf = p.confirm}
    <div class="emoji-pack-creator-confirm" id="emoji-pack-creator-confirm" data-tone={cf.tone || 'default'}>
        <div class="emoji-pack-creator-confirm-body">
            {#if cf.icon}<img class="emoji-pack-creator-confirm-icon" alt="" src="/icons/{cf.icon}">{/if}
            <p class="emoji-pack-creator-confirm-title">{cf.title || 'Are you sure?'}</p>
            {#if cf.detail}<p class="emoji-pack-creator-confirm-detail">{cf.detail}</p>{/if}
            <div class="emoji-pack-creator-confirm-actions">
                <button type="button" class="emoji-pack-creator-confirm-cancel btn" onclick={() => h.confirm(false)}>{cf.cancelText || 'CANCEL'}</button>
                <button type="button" class="emoji-pack-creator-confirm-ok btn" use:focusLater={false} onclick={() => h.confirm(true)}>{cf.confirmText || 'CONTINUE'}</button>
            </div>
        </div>
    </div>
{/if}

{#if p.progress}
    <div class="emoji-pack-creator-progress" id="emoji-pack-creator-progress">
        <div class="emoji-pack-creator-progress-body">
            <div class="emoji-pack-creator-progress-ring"></div>
            <p class="emoji-pack-creator-progress-title">{p.progress.title || 'Working…'}</p>
            <p class="emoji-pack-creator-progress-detail">{p.progress.detail || ''}</p>
        </div>
    </div>
{/if}

{#if p.naming}
    {@const n = p.naming}
    <div class="emoji-pack-creator-naming" id="emoji-pack-creator-naming">
        <div class="emoji-pack-creator-naming-body">
            {#if n.batch && n.batch.total > 1}
                <div class="emoji-pack-creator-naming-batch">{n.batch.current} of {n.batch.total}</div>
            {/if}
            <div class="emoji-pack-creator-naming-preview-wrap">
                <img class="emoji-pack-creator-naming-preview" alt="" use:preview={n.src}>
            </div>
            <p class="emoji-pack-creator-naming-title">{n.mode === 'edit' ? 'Rename Emoji' : 'Name This Emoji'}</p>
            <div class="emoji-pack-creator-naming-input-row">
                <span class="emoji-pack-creator-naming-colon">:</span>
                <input type="text" class="emoji-pack-creator-naming-input" id="emoji-pack-creator-naming-input" class:is-invalid={!!n.error}
                       autocomplete="off" autocorrect="off" autocapitalize="off"
                       spellcheck="false" maxlength="22" placeholder="shortcode"
                       bind:value={p.naming.value} use:focusLater={true}
                       oninput={() => h.namingInput()}
                       onkeydown={(e) => {
                           if (e.key === 'Enter') { e.preventDefault(); h.namingCommit(n.value); }
                           else if (e.key === 'Escape') { e.preventDefault(); h.namingCancel(); }
                       }}>
                <span class="emoji-pack-creator-naming-colon">:</span>
            </div>
            <p class="emoji-pack-creator-naming-error" hidden={!n.error}>{n.error}</p>
            <div class="emoji-pack-creator-naming-actions">
                <button type="button" class="emoji-pack-creator-naming-skip btn" onclick={() => h.namingCancel()}>{n.mode === 'edit' ? 'CANCEL' : 'SKIP'}</button>
                <button type="button" class="emoji-pack-creator-naming-save btn" onclick={() => h.namingCommit(n.value)}>SAVE</button>
            </div>
        </div>
    </div>
{/if}

<div class="emoji-pack-creator-cropper" id="emoji-pack-creator-cropper" hidden={!p.cropperOpen}>
    <div class="emoji-pack-creator-cropper-body">
        <p class="emoji-pack-creator-cropper-title">Crop to square</p>
        <p class="emoji-pack-creator-cropper-hint">Drag to move. Drag a corner to resize.</p>
        <div class="emoji-pack-creator-cropper-stage" bind:this={stage}>
            <img class="emoji-pack-creator-cropper-img" alt="" bind:this={img}>
            <div class="emoji-pack-creator-cropper-box" bind:this={box}>
                <div class="epcc-handle epcc-handle-tl" data-handle="tl"></div>
                <div class="epcc-handle epcc-handle-tr" data-handle="tr"></div>
                <div class="epcc-handle epcc-handle-bl" data-handle="bl"></div>
                <div class="epcc-handle epcc-handle-br" data-handle="br"></div>
            </div>
            <div class="epcc-preview" aria-hidden="true" bind:this={cropPreview}></div>
        </div>
        <div class="emoji-pack-creator-cropper-actions">
            <button type="button" class="emoji-pack-creator-cropper-cancel btn" bind:this={cancel}>CANCEL</button>
            <button type="button" class="emoji-pack-creator-cropper-ok btn" bind:this={ok}>CROP</button>
        </div>
    </div>
</div>

{#if p.error}
    {@const er = p.error}
    <div class="emoji-pack-creator-error" id="emoji-pack-creator-error">
        <div class="emoji-pack-creator-error-body">
            <div class="emoji-pack-creator-error-art">
                <img class="emoji-pack-creator-error-mascot" src="/icons/aggroboi.webp" alt="">
            </div>
            <p class="emoji-pack-creator-error-pretitle">{er.pretitle}</p>
            <p class="emoji-pack-creator-error-title">{er.title || 'Please Try Again.'}</p>
            <p class="emoji-pack-creator-error-detail">{er.detail}</p>
            <button type="button" class="emoji-pack-creator-error-retry btn" onclick={() => h.errorRetry()}>{er.button || 'TRY AGAIN'}</button>
        </div>
    </div>
{/if}
