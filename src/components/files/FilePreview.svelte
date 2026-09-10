<script>
    // The send-file preview overlay: what is about to be sent, its name (click to
    // rename), size, the image options and the buttons. The vanilla module owns the
    // file itself and the send; this derives the overlay from lib/filepreview.svelte.js.
    import { filePreview, filePreviewContent } from '../lib/filepreview.svelte.js';

    let { h } = $props();   // h: FilePreviewHelpers (js/file-preview.js)

    const fp = filePreview();
    const content = $derived(filePreviewContent());

    // Mounted while open and for the 200ms fade after; the `active` class drives the CSS transition.
    let shown = $state(false);
    let active = $state(false);
    let overlay = $state(null);
    $effect(() => {
        if (fp.open) {
            shown = true;
            const t = setTimeout(() => { active = true; }, 10);
            return () => clearTimeout(t);
        }
        active = false;
        const t = setTimeout(() => { shown = false; }, 200);
        return () => clearTimeout(t);
    });

    // A video keeps playing under a closed overlay unless stopped.
    $effect(() => {
        if (fp.open || !overlay) return;
        const video = overlay.querySelector('video');
        if (video) { video.pause(); video.src = ''; video.load(); }
    });

    // Keyboard: Escape closes, Enter sends (unless the send is disabled).
    $effect(() => {
        if (!fp.open) return;
        const onKey = (e) => {
            if (e.key === 'Escape') h.close();
            else if (e.key === 'Enter') {
                if (fp.sendDisabled) return;
                e.preventDefault();
                h.send();
            }
        };
        document.addEventListener('keydown', onKey);
        return () => document.removeEventListener('keydown', onKey);
    });

    // ── the name: click to rename (stem only; the extension badge is read-only) ──
    let editing = $state(false);
    let draft = $state('');
    function startEdit() {
        draft = fp.stem;
        editing = true;
    }
    function focusSelect(node) {
        node.focus();
        node.select();
    }
    function onDraftInput(e) {
        const input = e.currentTarget;
        const pos = input.selectionStart;
        const before = input.value;
        const clean = h.sanitizeStem(before);
        if (clean !== before) {
            input.value = clean;
            const diff = before.length - clean.length;
            input.setSelectionRange(pos - diff, pos - diff);
        }
        draft = clean;
    }
    function saveEdit() {
        if (!editing) return;
        editing = false;
        const name = draft.trim();
        if (name) { fp.stem = name; fp.edited = true; }
    }
    function onDraftKey(e) {
        if (e.key === 'Enter') { e.preventDefault(); e.stopPropagation(); e.currentTarget.blur(); }
        else if (e.key === 'Escape') { e.stopPropagation(); editing = false; }
    }

    // ── the image: a fallback read when the asset protocol refuses the path, and the spoiler ──
    function imgFallback(e) {
        const img = e.currentTarget;
        const path = content?.path;
        if (!path) return;
        img.onerror = null;
        h.readImagePreview(path).then((src) => { img.src = src; }).catch(() => {});
    }

    // Spoiler: swap the image for its thumbhash at the same rendered size, with a
    // "Spoiler" label when there is room; off restores the original.
    let imgContainer = $state(null);
    let originalSrc = null;
    let thumbhashSrc = null;
    function applySpoiler(on) {
        const container = imgContainer;
        const img = container?.querySelector('.file-preview-image');
        if (!container || !img) return;
        if (on) {
            originalSrc = img.src;
            const rect = img.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) {
                img.style.width = rect.width + 'px';
                img.style.height = rect.height + 'px';
            } else {
                img.width = img.naturalWidth;
                img.height = img.naturalHeight;
                img.style.height = 'auto';
            }
            img.style.maxWidth = 'none';
            img.style.maxHeight = 'none';
            img.style.objectFit = 'fill';
            if (thumbhashSrc) {
                img.src = thumbhashSrc;
            } else {
                h.thumbhash(content?.path || '')
                    .then((dataUrl) => { thumbhashSrc = dataUrl; if (fp.spoiler) img.src = dataUrl; })
                    .catch(() => { if (fp.spoiler) img.classList.add('spoiler-blur'); });
            }
            const w = rect.width || img.naturalWidth;
            const hh = rect.height || img.naturalHeight;
            if (w >= 80 && hh >= 60 && !container.querySelector('.spoiler-overlay')) {
                const overlayEl = document.createElement('div');
                overlayEl.className = 'spoiler-overlay';
                overlayEl.innerHTML = '<span class="icon icon-eye-off"></span><span class="spoiler-label">Spoiler</span>';
                container.appendChild(overlayEl);
            }
        } else {
            img.classList.remove('spoiler-blur');
            if (originalSrc) img.src = originalSrc;
            img.style.width = '';
            img.style.height = '';
            img.style.maxWidth = '';
            img.style.maxHeight = '';
            img.style.objectFit = '';
            img.removeAttribute('width');
            img.removeAttribute('height');
            container.querySelector('.spoiler-overlay')?.remove();
        }
    }
    // A new image resets the cached sources; then the flag drives the swap.
    $effect(() => { content; originalSrc = null; thumbhashSrc = null; });
    $effect(() => {
        const on = fp.spoiler;
        if (!imgContainer) return;
        applySpoiler(on);
    });
    function toggleSpoiler(e) {
        e.stopPropagation();
        fp.spoiler = !fp.spoiler;
    }

    // ── the zip tree: the vanilla builder, with its collapsible toggles ──
    function zipTree(node, c) {
        node.innerHTML = h.buildFileListHtml(c.files, c.total);
        h.initFileTreeToggles();
    }
</script>

{#if shown}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions (byte-identical to the vanilla overlay) -->
    <div class="file-preview-overlay" class:active bind:this={overlay} style="display: flex;" onclick={(e) => { if (e.target === overlay) h.close(); }}>
        <div class="file-preview-container">
            <div class="file-preview-inner">
                <div id="file-preview-content">
                    {#if content?.kind === 'image'}
                        <div class="file-preview-image-container" bind:this={imgContainer}>
                            <img src={content.src} class="file-preview-image" alt="Preview" onerror={imgFallback} />
                            <button class="spoiler-toggle" type="button" title={fp.spoiler ? 'Remove spoiler' : 'Mark as spoiler'} onclick={toggleSpoiler}>
                                <span class="icon {fp.spoiler ? 'icon-eye-off' : 'icon-eye'}"></span>
                            </button>
                        </div>
                    {:else if content?.kind === 'video'}
                        <div class="file-preview-video-container">
                            <!-- svelte-ignore a11y_media_has_caption -->
                            <video src={content.src} class="file-preview-video" controls muted></video>
                        </div>
                    {:else if content?.kind === 'miniapp'}
                        {#if content.icon}
                            <div class="file-preview-image-container file-preview-miniapp">
                                <img src={content.icon} class="file-preview-image file-preview-miniapp-icon" alt={content.name || 'Mini App'} />
                            </div>
                        {:else}
                            <div class="file-preview-icon-container file-preview-miniapp">
                                <div class="icon icon-file file-preview-icon"></div>
                                <span class="file-preview-miniapp-badge">Mini App</span>
                            </div>
                        {/if}
                    {:else if content?.kind === 'zip-progress'}
                        <div class="file-preview-icon-container">
                            <div class="zip-progress-spinner" style="--progress: {content.percent}%"></div>
                        </div>
                        <div class="file-preview-zip-label">Compressing...{content.percent > 0 ? ` ${content.percent}%` : ''}</div>
                    {:else if content?.kind === 'zip'}
                        <div class="file-preview-icon-container">
                            <div class="icon icon-folder file-preview-icon"></div>
                        </div>
                        {#key content}
                            <div style="display: contents" use:zipTree={content}></div>
                        {/key}
                    {:else if content?.kind === 'icon'}
                        <div class="file-preview-icon-container">
                            <div class="icon {content.icon} file-preview-icon"></div>
                        </div>
                    {/if}
                </div>
                <div class="file-preview-info">
                    <div class="file-preview-name-row">
                        {#if editing}
                            <input type="text" class="file-preview-name-input" maxlength="64" value={draft} use:focusSelect oninput={onDraftInput} onblur={saveEdit} onkeydown={onDraftKey} />
                        {:else}
                            <div class="file-preview-name" id="file-preview-name" onclick={startEdit}>{fp.stem}</div>
                        {/if}
                        <span class="file-preview-ext-badge" id="file-preview-ext" style:display={fp.ext ? '' : 'none'}>{fp.ext ? `.${fp.ext}` : ''}</span>
                    </div>
                    <div class="file-preview-details">
                        <span class="file-preview-detail" id="file-preview-size">{fp.size}</span>
                    </div>
                </div>
                <div class="file-preview-options" id="file-preview-options">
                    {#if fp.compress}
                        <label class="file-preview-option">
                            <div>
                                <div class="file-preview-option-label">Compress Image</div>
                                <div class="file-preview-option-sublabel" id="file-preview-compress-info">{fp.compressInfo}</div>
                            </div>
                            <input type="checkbox" id="file-preview-compress" bind:checked={fp.compressChecked} />
                            <span class="neon-toggle"></span>
                        </label>
                    {/if}
                    {#if fp.metadata}
                        <!-- Shown only when the image carries strip-worthy EXIF; the warning
                             surfaces while Keep Metadata is on (location, camera, date leave the device). -->
                        <label class="file-preview-option" id="file-preview-metadata-option">
                            <div>
                                <div class="file-preview-option-label">Keep Metadata <img class="warning-icon" id="file-preview-metadata-warning" alt="" style="vertical-align: middle; margin-left: 4px; height: 14px; width: 14px;" style:display={fp.metadataChecked ? 'inline-block' : 'none'} /></div>
                                <div class="file-preview-option-sublabel">Includes location, camera &amp; date</div>
                            </div>
                            <input type="checkbox" id="file-preview-metadata" bind:checked={fp.metadataChecked} />
                            <span class="neon-toggle"></span>
                        </label>
                    {/if}
                </div>
            </div>
            <div class="file-preview-buttons">
                {#if fp.publish}
                    <button class="file-preview-btn file-preview-btn-publish" id="file-preview-publish" style="display: flex;" onclick={h.publish}>
                        <span class="icon icon-star"></span> Publish
                    </button>
                {/if}
                <button class="file-preview-btn file-preview-btn-cancel" id="file-preview-cancel" onclick={h.close}>Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="file-preview-send" disabled={fp.sendDisabled} onclick={h.send}>{fp.sendLabel}</button>
            </div>
        </div>
    </div>
{/if}
