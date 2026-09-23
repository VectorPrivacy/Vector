<script>
    // A file attachment as a box: icon, name, and a second line that says what the file is
    // doing — a bar while bytes move, a description at rest. The box itself is the action;
    // the button on the right only names it. Transfer state comes from the store; a Mini App
    // shows its realtime session instead of a description.
    //
    // The chrome is deliberately grey. The only colour in the box is the one carrying meaning:
    // a file that is on this device takes the theme colour, a failed one takes the danger colour.
    import { transfer, transferSlow, transferStageText } from '../../lib/attachments.svelte.js';
    import { miniappStatus } from '../../lib/miniapps.svelte.js';
    import { profileVersion } from '../../lib/signals.svelte.js';
    import { messageVersion } from '../../lib/chatview.svelte.js';

    let { att, msg, sender = null, phase, label = null, failed = false, onActivate = null, h } = $props();
    // phase: 'downloaded' | 'download' | 'downloading'; label overrides the second line,
    // failed paints the failed state for a caller whose attachment carries no flag,
    // onActivate overrides the click
    // h: fileTypeInfo(ext), formatBytes(n, dec?, short?), assetUrl(path), loadMiniAppInfo(path), marketplaceApp(hash),
    //    backendCachedImg(img, url), openFile(att, msg), startDownload(att, msg, sender),
    //    cancelDownload(att, msg), cancelUpload(id), pausedAt(att),
    //    getProfile, getProfileAvatarSrc, showTooltip, hideTooltip

    // Mount-time: an attachment's kind never changes under its box.
    // svelte-ignore state_referenced_locally
    const ext = (att.extension || '').toLowerCase();
    // svelte-ignore state_referenced_locally
    const info = h.fileTypeInfo(ext);
    const isMiniApp = info.isMiniApp === true;
    // svelte-ignore state_referenced_locally
    const topicKey = att.webxdc_topic || `att:${att.id}`;

    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending) && phase === 'downloaded'; });
    const up = $derived(uploading ? transfer(msg.id) : null);
    const down = $derived(phase === 'downloading' ? transfer(att.id) : null);
    const moving = $derived(uploading || phase === 'downloading');
    const pct = $derived(moving ? (uploading ? up?.pct : down?.pct) : null);
    // Part of the file is already here: the next download picks up from it.
    const paused = $derived.by(() => { messageVersion(msg.id); return moving || phase === 'downloaded' ? null : h.pausedAt?.(att) || null; });
    // One word for the whole box: the stylesheet colours from it, nothing else has to agree.
    const state = $derived(
        uploading ? 'uploading'
        : phase === 'downloading' ? 'downloading'
        : phase === 'downloaded' ? 'local'
        : paused ? 'paused'
        : (failed || att.download_failed) ? 'failed'
        : 'remote'
    );

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
        return `· ${h.formatBytes(t.bps, t.bps >= 1048576 ? 2 : 0, true)}/s`;
    });
    // The rate's slot says what the transfer is doing whenever no bytes move: sealing
    // before an upload, opening after a download, or a publish that drags on.
    const note = $derived.by(() => {
        const text = uploading && transferSlow(msg.id) ? 'Sending'
            : uploading ? transferStageText(msg.id)
            : phase === 'downloading' ? transferStageText(att.id)
            : '';
        // The same separator the rate carries, so the line reads the same either way.
        return text ? `· ${text}` : '';
    });
    // What the file IS, for the line below the name: its own extension, or the closest word we have.
    const kind = $derived(ext ? `.${ext}` : info.description);
    const sizeText = $derived(att.size > 0 ? h.formatBytes(att.size) : '');
    const restLine = $derived(sizeText ? `${kind} — ${sizeText}` : kind);
    const pausedLine = $derived(paused
        ? `Paused at ${h.formatBytes(paused.offset)}${paused.total ? ` of ${h.formatBytes(paused.total)}` : ''} · Tap to Resume`
        : '');
    const failLine = $derived.by(() => {
        const reason = (att.download_error || '').slice(0, 64);
        return reason ? `Failed: ${reason} · Tap to Retry` : 'Download Failed · Tap to Retry';
    });


    function click() {
        if (onActivate) { if (phase !== 'downloading') onActivate(); return; }
        if (phase === 'downloaded') h.openFile(att, msg);
        else if (phase === 'download') h.startDownload(att, msg, sender);
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="file-attachment" filepath={att.path || undefined} class:miniapp-attachment={isMiniApp} data-webxdc-topic={att.webxdc_topic || undefined}
     data-playing={rt?.active ? 'true' : undefined} onclick={click}>
    <div class="file-box" data-state={state} class:is-static={phase === 'downloading' || rt?.active}>
        <span class="file-box-icon">
            {#if isMiniApp && iconRemote && !iconSrc}
                <img alt="" use:remoteIcon={iconRemote}>
            {:else if isMiniApp && iconSrc}
                <img alt="" src={iconSrc}>
            {:else if isMiniApp}
                <!-- No package icon yet: the controller is what Mini Apps are called everywhere else. -->
                <span class="icon icon-gamepad"></span>
            {:else}
                <span class="icon icon-{info.icon}" data-attachment-id={phase === 'download' ? att.id : undefined}></span>
            {/if}
        </span>
        <span class="file-box-text">
            <span class="file-box-title cutoff">
                <!-- The bar says it is moving, so the title line only says how big and how fast. -->
                {title}{#if moving}<span class="file-box-meta">{sizeText ? ` — ${sizeText}` : ''}<span class="file-box-rate" class:is-visible={!!(note || speed)}>{note || speed}</span></span>{/if}
            </span>
            {#if moving}
                <span class="file-box-bar" data-attachment-id={phase === 'downloading' ? att.id : undefined}
                      id={uploading ? `${msg.id}_file` : undefined} style:--progress={pct != null ? `${pct}%` : null}></span>
            {:else if state === 'local' && isMiniApp}
                <span class="file-box-session">
                    <span style:color={rt?.active ? 'var(--icon-color-primary)' : 'var(--file-box-body)'}>{playLabel}</span>
                    {#if players > 0}
                        <span class="file-box-peers">
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
                </span>
            {:else}
                <span class="file-box-sub">{label ?? (state === 'paused' ? pausedLine : state === 'failed' ? failLine : restLine)}</span>
            {/if}
        </span>
        {#if uploading && up?.phase !== 'publishing'}
            <button class="file-box-action is-cancel" aria-label="Cancel upload"
                    onclick={(e) => { e.stopPropagation(); h.cancelUpload(msg.id); }}>
                <span class="icon icon-x"></span>
            </button>
        {:else if state === 'downloading' && !onActivate}
            <!-- Only a real attachment download can be stopped; a synthetic card has no walk to call off. -->
            <button class="file-box-action is-cancel" aria-label="Cancel download"
                    onclick={(e) => { e.stopPropagation(); h.cancelDownload(att, msg); }}>
                <span class="icon icon-x"></span>
            </button>
        {:else if state === 'paused'}
            <button class="file-box-action" aria-label="Resume download"
                    onclick={(e) => { e.stopPropagation(); click(); }}>
                <span class="icon icon-download"></span>
            </button>
        {:else if state === 'failed'}
            <button class="file-box-action is-failed" aria-label="Retry download"
                    onclick={(e) => { e.stopPropagation(); click(); }}>
                <span class="icon icon-refresh"></span>
            </button>
        {:else if state === 'remote'}
            <button class="file-box-action" aria-label="Download"
                    onclick={(e) => { e.stopPropagation(); click(); }}>
                <span class="icon icon-download"></span>
            </button>
        {/if}
    </div>
</div>
