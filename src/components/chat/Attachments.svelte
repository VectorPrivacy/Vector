<script>
    // A message's attachments, each a component over transfer state. Auto-download fires
    // once per attachment from here, deduplicated by the app.
    import ImageAttachment from './attachments/ImageAttachment.svelte';
    import AudioPlayer from './attachments/AudioPlayer.svelte';
    import VideoAttachment from './attachments/VideoAttachment.svelte';
    import ThumbhashAttachment from './attachments/ThumbhashAttachment.svelte';
    import FileBox from './attachments/FileBox.svelte';
    import ModelAttachment from './attachments/ModelAttachment.svelte';
    import { MODEL_EXTENSIONS, modelsSupported } from '../lib/model3d.js';
    import { messageVersion } from '../lib/chatview.svelte.js';
    import { attachmentReady, nearScreen } from '../lib/media.js';

    let { msg, sender, ctx, h } = $props();
    // h: MediaHelpers (js/render/chat/message-row.js); `h.audio` is AudioPlayer's own bag

    const IMAGE_BLUR = ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

    // Transfer state is mutated in place on the attachment; the message's version is
    // what moves, so every phase derives under it.
    const rows = $derived.by(() => {
        messageVersion(msg.id);
        return (msg.attachments || []).map((att) => ({
            att,
            phase: attachmentReady(att) ? 'downloaded' : h.isDownloading(att) ? 'downloading' : 'idle',
            auto: !attachmentReady(att) && h.willAutoDownload(att, ctx),
            uploading: !!(msg.mine && msg.pending),
        }));
    });

    // Auto-download: a side effect, once per attachment id (the app dedupes across renders),
    // and only once the message comes near the screen: a long chat loads what you scroll to.
    // A file that came without a size asks for it then too: auto-download needs it.
    let near = $state(false);
    const unsized = (r) => r.phase === 'idle' && !(r.att.size > 0) && !ctx.held && !ctx.revealedBlocked;
    const waiting = $derived(rows.some((r) => (r.phase === 'idle' && r.auto) || unsized(r)));
    $effect(() => {
        if (!near) return;
        for (const r of rows) {
            if (r.phase === 'idle' && r.auto) h.autoDownload(r.att, msg, sender);
            else if (unsized(r)) h.probeSize(r.att, msg);
        }
    });
</script>

{#if waiting && !near}<span class="att-near" aria-hidden="true" use:nearScreen={() => (near = true)}></span>{/if}
<!-- By position, not id: an upload's id changes when it lands, and its picture must not remount. -->
{#each rows as { att, phase, auto }, i (i)}
    {#if phase === 'downloaded'}
        {#if h.isImage(att.extension)}
            <ImageAttachment {att} {msg} {ctx} {sender} {h} />
        {:else if h.isAudio(att.extension)}
            <AudioPlayer {att} {msg} h={h.audio} />
        {:else if h.isVideo(att.extension)}
            <VideoAttachment {att} {msg} {h} />
        {:else if MODEL_EXTENSIONS.includes((att.extension || '').toLowerCase()) && modelsSupported()}
            <ModelAttachment {att} {msg} {sender} {h} />
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
            <FileBox {att} {msg} {sender} phase={auto ? 'downloading' : 'download'} held={!!ctx.held} {h} />
        {/if}
    {/if}
{/each}
