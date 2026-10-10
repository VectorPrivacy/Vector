<script>
    // The OpenGraph card under a message with a link: favicon and title, the description
    // with its line breaks, the image. Both images come through the backend cache: the
    // linked host is attacker-chosen, and a raw src would be a clearnet fetch past Tor.
    // A video link's picture plays the video in place, and only once it is clicked.
    import { playersState, playersAvailable, reservePlayer, startPlayer, stopPlayer } from '../lib/players.svelte.js';
    import { createFrame, adoptFrame, ownsFrame, placeFrame, unplaceFrame, dropFrame } from '../lib/framehost.js';
    import { popOut, takeBack, popoutPlayingAudio } from '../lib/popout.svelte.js';

    let { data, h } = $props();   // data: linkPreviewData(msg); h: backendCachedImg(img, url), onThumbLoad(), openUrl(url), openChat(), playerUrl(video)

    // Description text arrives with <br> and newline breaks; both render as line breaks.
    const lines = $derived((data.description || '').split(/<br\s*\/?>/i).flatMap((part, i, parts) => {
        const subs = part.split('\n');
        return i < parts.length - 1 ? [...subs, ''] : subs;
    }));

    let faviconHidden = $state(false);
    let imageHidden = $state(false);

    // The player lives in lib/framehost.js, laid over this card: leaving the chat hands it to the
    // floating player still playing, and coming back takes it back, so it never reloads.
    const players = playersState();
    const canPlay = $derived(!!data.video && playersAvailable());
    // Mount-time: a row's chat never changes under it.
    // svelte-ignore state_referenced_locally
    const chatId = h.openChat();
    const owner = {};
    // svelte-ignore state_referenced_locally
    const back = data.video ? takeBack(data.msgId, 'frame', chatId) : null;
    let frameKey = $state(back && adoptFrame(back.frameKey, owner) ? back.frameKey : '');
    const playing = $derived(canPlay && !!frameKey && players.playing === frameKey);
    // Another player starting, the setting off, or the network leaving Clearnet: gone at once.
    $effect(() => {
        if (!frameKey || playing) return;
        if (ownsFrame(frameKey, owner)) dropFrame(frameKey);
        stopPlayer(frameKey);
        frameKey = '';
    });

    function cached(img, url) {
        h.backendCachedImg(img, url);
    }
    function onLoad(img) {
        if (img.isConnected) h.onThumbLoad();
    }

    async function play(e) {
        e.stopPropagation();
        if (!canPlay) return h.openUrl(data.url);
        const token = Symbol();
        reservePlayer(token);
        const url = await h.playerUrl(data.video);
        if (!url || players.playing !== token) return;
        const key = createFrame(url, data.title, owner);
        frameKey = key;
        startPlayer(key);
    }

    // The chat's list bounds what shows of it; the drawer and buttons over the list cut through.
    function place(node, key) {
        const list = node.closest('.chat-messages');
        const chat = node.closest('.chat');
        placeFrame(key, {
            box: node,
            clip: list,
            radius: '4px',
            holes: () => chat ? chat.querySelectorAll('.pins-drawer, .scroll-return-btn') : [],
        });
        return {
            destroy() {
                if (!ownsFrame(key, owner)) return;
                // Leaving with it playing: on to the floating player, unless music holds it.
                if (players.playing === key && playersAvailable() && !popoutPlayingAudio()) {
                    unplaceFrame(key, () => stopPlayer(key));
                    if (popOut({ kind: 'frame', id: data.msgId, chatId, msg: { id: data.msgId }, frameKey: key, title: data.title })) return;
                }
                dropFrame(key);
                stopPlayer(key);
            },
        };
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="dmsg-preview btn" url={data.url} style:padding-bottom={data.description && data.image ? '0' : null} onclick={() => h.openUrl(data.url)}>
    <span>
        <img class="favicon" alt="" style:display={faviconHidden ? 'none' : null} use:cached={data.favicon}
             onload={(e) => onLoad(e.currentTarget)} onerror={() => { faviconHidden = true; }}>{data.title}</span>
    {#if data.description}
        <span class="dmsg-preview-description" style:border-radius={data.image ? '0' : null}>
            {#each lines as line, i}{line}{#if i < lines.length - 1}<br>{/if}{/each}
        </span>
    {/if}
    {#if data.video}
        <!-- A picture that fails leaves a black box: the video can still play. -->
        <div class="dmsg-preview-video">
            {#if playing}
                <div class="dmsg-preview-player" use:place={frameKey}></div>
            {:else}
                {#if data.image && !imageHidden}
                    <img class="dmsg-preview-img" alt="" use:cached={data.image}
                         onload={(e) => onLoad(e.currentTarget)} onerror={() => { imageHidden = true; }}>
                {/if}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="dmsg-preview-play" class:external={!canPlay} onclick={play}>
                    <div class="dmsg-preview-play-btn"><div class="icon {canPlay ? 'icon-play' : 'icon-external'}"></div></div>
                    <div class="dmsg-preview-play-label">{canPlay ? 'Plays from YouTube' : 'Opens in your browser'}</div>
                </div>
            {/if}
        </div>
    {:else if data.image && !imageHidden}
        <img class="dmsg-preview-img" alt="" use:cached={data.image}
             onload={(e) => onLoad(e.currentTarget)} onerror={() => { imageHidden = true; }}>
    {/if}
</div>
