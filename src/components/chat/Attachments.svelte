<script>
    // A message's attachments, each a component over transfer state. Auto-download fires
    // once per attachment from here, deduplicated by the app.
    import ImageAttachment from './attachments/ImageAttachment.svelte';
    import AudioPlayer from './attachments/AudioPlayer.svelte';
    import VideoAttachment from './attachments/VideoAttachment.svelte';
    import ThumbhashAttachment from './attachments/ThumbhashAttachment.svelte';
    import FileBox from './attachments/FileBox.svelte';
    import { messageVersion } from '../lib/chatview.svelte.js';

    let { msg, sender, ctx, h } = $props();
    // h (beyond the leaves'): isImage(ext), isAudio(ext), isVideo(ext), willAutoDownload(att, ctx), isDownloading(att),
    //    audio (the player's bag), autoDownload(att, msg, sender), and FileBox's

    const IMAGE_BLUR = ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

    // Transfer state is mutated in place on the attachment; the message's version is
    // what moves, so every phase derives under it.
    const rows = $derived.by(() => {
        messageVersion(msg.id);
        return (msg.attachments || []).map((att) => ({
            att,
            phase: att.downloaded ? 'downloaded' : h.isDownloading(att) ? 'downloading' : 'idle',
            auto: !att.downloaded && h.willAutoDownload(att, ctx),
            uploading: !!(msg.mine && msg.pending),
        }));
    });

    // Auto-download: a side effect, once per attachment id (the app dedupes across renders).
    $effect(() => {
        for (const r of rows) {
            if (r.phase === 'idle' && r.auto) h.autoDownload(r.att, msg, sender);
        }
    });
</script>

<!-- By position, not id: an upload's id changes when it lands, and its picture must not remount. -->
{#each rows as { att, phase, auto }, i (i)}
    {#if phase === 'downloaded'}
        {#if h.isImage(att.extension)}
            <ImageAttachment {att} {msg} {ctx} {sender} {h} />
        {:else if h.isAudio(att.extension)}
            <AudioPlayer {att} {msg} h={h.audio} />
        {:else if h.isVideo(att.extension)}
            <VideoAttachment {att} {msg} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase="downloaded" {h} />
        {/if}
    {:else if phase === 'downloading'}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} auto={false} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase="downloading" {h} />
        {/if}
    {:else}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} {auto} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase={auto ? 'downloading' : 'download'} {h} />
        {/if}
    {/if}
{/each}
