<script>
    // A file attachment as a box: icon, name, and a small line that says what the file is
    // doing. Transfer state comes from the store; a Mini App shows its realtime session.
    import { transfer } from '../../lib/attachments.svelte.js';
    import { miniappStatus } from '../../lib/miniapps.svelte.js';
    import { profileVersion } from '../../lib/signals.svelte.js';

    let { att, msg, sender = null, phase, label = null, onActivate = null, h } = $props();
    // phase: 'downloaded' | 'download' | 'downloading'; label overrides the small line; onActivate overrides the click
    // h: fileTypeInfo(ext), formatBytes(n, dec?, short?), assetUrl(path), loadMiniAppInfo(path), marketplaceApp(hash),
    //    backendCachedImg(img, url), openFile(att, msg), startDownload(att, msg, sender), cancelUpload(id),
    //    getProfile, getProfileAvatarSrc, showTooltip, hideTooltip

    // Mount-time: an attachment's kind never changes under its box.
    // svelte-ignore state_referenced_locally
    const ext = (att.extension || '').toLowerCase();
    // svelte-ignore state_referenced_locally
    const info = h.fileTypeInfo(ext);
    const isMiniApp = info.isMiniApp === true;
    // svelte-ignore state_referenced_locally
    const topicKey = att.webxdc_topic || `att:${att.id}`;

    const uploading = $derived(msg.mine && msg.pending && phase === 'downloaded');
    const up = $derived(uploading ? transfer(msg.id) : null);
    const down = $derived(phase === 'downloading' ? transfer(att.id) : null);
    const spinning = $derived(uploading || phase === 'downloading');
    const pct = $derived(spinning ? (uploading ? up?.pct : down?.pct) : null);

    // Name and icon: the file's own, or resolved from the Mini App package / the marketplace.
    // svelte-ignore state_referenced_locally
    let title = $state(att.name || info.description);
    let iconSrc = $state(null);
    let iconRemote = $state(null);
    $effect(() => {
        if (!isMiniApp) return;
        if (phase === 'downloaded' && att.path) {
            h.loadMiniAppInfo(att.path).then((i) => { if (i) { title = i.name || 'Mini App'; if (i.icon_data) iconSrc = i.icon_data; } }).catch(() => {});
        } else if (phase !== 'downloaded') {
            h.marketplaceApp(att.id).then((app) => {
                if (!app) return;
                if (app.name) title = app.name;
                if (app.icon_cached) iconSrc = h.assetUrl(app.icon_cached);
                else if (app.icon_url) iconRemote = app.icon_url;
            }).catch(() => {});
        }
    });
    function remoteIcon(img, url) { if (url) h.backendCachedImg(img, url); return { update: (u) => { if (u) h.backendCachedImg(img, u); } }; }

    // The realtime session: the store's word, once a status has landed.
    const rt = $derived(isMiniApp && phase === 'downloaded' ? miniappStatus(topicKey) : null);
    const players = $derived.by(() => {
        if (!rt) return 0;
        return rt.peers?.length > 0 ? rt.peers.length : rt.peerCount;
    });
    const playLabel = $derived(rt?.active ? 'Playing' : (players > 0 ? 'Click to Join' : 'Click to Play'));
    const peerAvatars = $derived.by(() => {
        const npubs = [...(rt?.peers || [])].sort().slice(0, 5);
        return npubs.map((npub) => {
            profileVersion(npub);
            const p = h.getProfile(npub);
            return { npub, src: h.getProfileAvatarSrc(p) || 'icons/user-placeholder.svg', name: p?.nickname || p?.name || p?.display_name || '' };
        });
    });

    const speed = $derived.by(() => {
        const t = uploading ? up : down;
        if (!t || !(t.bps > 0)) return '';
        return ` · ${h.formatBytes(t.bps, t.bps >= 1048576 ? 2 : 0, true)}/s`;
    });
    const small = $derived.by(() => {
        if (label) return label;
        if (phase === 'downloading') return `Downloading${speed}`;
        if (phase === 'download') {
            if (att.download_failed) {
                const reason = (att.download_error || '').slice(0, 64);
                return reason ? `Failed: ${reason} · Tap to Retry` : 'Download Failed · Tap to Retry';
            }
            return `Click to Download${att.size > 0 ? ` · ${h.formatBytes(att.size)}` : ''}`;
        }
        return null;
    });
    const sizeText = $derived(uploading ? `Uploading${speed}` : h.formatBytes(att.size));

    function click() {
        if (onActivate) { if (phase !== 'downloading') onActivate(); return; }
        if (phase === 'downloaded') h.openFile(att, msg);
        else if (phase === 'download') h.startDownload(att, msg, sender);
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div filepath={att.path || undefined} class:miniapp-attachment={isMiniApp} data-webxdc-topic={att.webxdc_topic || undefined}
     data-playing={rt?.active ? 'true' : undefined} onclick={click}
     style:cursor={phase === 'downloading' || rt?.active ? 'default' : (isMiniApp ? 'pointer' : null)}>
    <div class="custom-audio-player" class:btn={!isMiniApp} style="display: flex; align-items: center; padding: 10px; padding-right: 15px; position: relative;">
        {#if spinning}
            <div class="miniapp-downloading-spinner" data-attachment-id={phase === 'downloading' ? att.id : undefined} id={uploading ? `${msg.id}_file` : undefined}
                 style="position: absolute; left: 15px; top: 0; bottom: 0; margin: auto; width: 40px; height: 40px; transition: --progress 0.3s ease;"
                 style:--progress={pct != null ? `${pct}%` : null}></div>
        {:else if isMiniApp}
            {#if iconRemote && !iconSrc}
                <img alt="" style="margin-left: 5px; width: 40px; height: 40px; border-radius: 8px; object-fit: cover; background-color: transparent;" use:remoteIcon={iconRemote}>
            {:else}
                <img alt="" src={iconSrc || 'data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 24 24%22 fill=%22%23fff%22><path d=%22M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z%22/></svg>'}
                     style="margin-left: 5px; width: 40px; height: 40px; border-radius: 8px; object-fit: cover; background-color: transparent;">
            {/if}
        {:else}
            <span class="icon icon-{info.icon}" data-attachment-id={phase === 'download' ? att.id : undefined} style="margin-left: 5px; width: 50px; background-color: rgba(255, 255, 255, 0.75);"></span>
        {/if}
        <span style="color: rgba(255, 255, 255, 0.85); line-height: 1.2; min-width: 0;" style:margin-left={spinning || !isMiniApp ? '60px' : '15px'}>
            <span class="cutoff" style="color: var(--icon-color-primary); font-weight: 400;">{title}</span>
            {#if phase === 'downloaded' && isMiniApp}
                <small style="display: flex; align-items: center; gap: 10px;">
                    <span style="font-weight: 400;" style:color={rt?.active ? '#2ed573' : 'rgba(255, 255, 255, 0.7)'}>{playLabel}</span>
                    {#if players > 0}
                        <span style="padding: 2px 8px; border-radius: 10px; background-color: rgba(46, 213, 115, 0.15); color: rgb(46, 213, 115); font-size: 0.85em; font-weight: 500; border: 0.5px solid rgba(46, 213, 115, 0.3); display: inline-flex; align-items: center;">
                            {#if peerAvatars.length}
                                <span style="display: inline-flex; align-items: center; margin-right: 4px;">
                                    {#each peerAvatars as p, i (p.npub)}
                                        <!-- svelte-ignore a11y_no_static_element_interactions -->
                                        <span style="position: relative;" style:z-index={peerAvatars.length - i} style:margin-left={i > 0 ? '-5px' : null}
                                              onmouseenter={p.name ? (e) => h.showTooltip(p.name, e.currentTarget) : null} onmouseleave={p.name ? () => h.hideTooltip() : null}>
                                            <img src={p.src} alt="" style="width: 14px; height: 14px; border-radius: 50%; border: 1px solid #1a1a2e; object-fit: cover; display: block;" onerror={(e) => { e.currentTarget.onerror = null; e.currentTarget.src = 'icons/user-placeholder.svg'; }}>
                                        </span>
                                    {/each}
                                </span>
                            {:else}
                                <img src="icons/group-placeholder.svg" alt="" style="width: 14px; height: 14px; vertical-align: middle; margin-right: 4px;">
                            {/if}
                            {players} online
                        </span>
                    {/if}
                </small>
            {:else if phase === 'downloaded'}
                <small>
                    {#if att.name}
                        <span class="file-attach-size">{sizeText}</span>
                    {:else}
                        <span style="color: white; font-weight: 400;">.{ext}</span><span class="file-attach-size"> — {sizeText}</span>
                    {/if}
                </small>
            {:else}
                <small><span class="file-status" style="color: rgba(255, 255, 255, 0.7); font-weight: 400;">{small}</span></small>
            {/if}
        </span>
        {#if uploading}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="upload-cancel-btn" onclick={(e) => { e.stopPropagation(); h.cancelUpload(msg.id); }}></div>
        {/if}
    </div>
</div>
