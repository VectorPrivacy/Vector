<script>
    // The toolbar's buttons, from toolbar state. The host element, its placement and
    // the click dispatch stay in the app: it reads each button's data attributes.
    import { messageToolbar } from '../lib/toolbar.svelte.js';

    const tb = $derived(messageToolbar());
    const show = $derived(tb.show || {});
    const del = $derived(tb.del);
    const deleteLabel = $derived(del?.label || 'Delete message');
</script>

<button class="dmsg-toolbar-btn btn" data-action="react" aria-label="Add reaction" title="Add reaction" hidden={!show.react}><span class="icon icon-smile-face"></span></button>
<button class="dmsg-toolbar-btn btn" data-action="reply" aria-label="Reply" title="Reply" hidden={!show.reply}><span class="icon icon-reply"></span></button>
<button class="dmsg-toolbar-btn btn" data-action="edit" aria-label="Edit" title="Edit" hidden={!show.edit}><span class="icon icon-edit"></span></button>
<button class="dmsg-toolbar-btn btn" data-action="reveal-file" aria-label="Reveal in folder" title="Reveal in folder" hidden={!show.reveal} data-path={show.reveal ? tb.path : null}><span class="icon icon-file-search"></span></button>
<button class="dmsg-toolbar-btn btn" data-action="copy-file" aria-label="Copy" title="Copy" hidden={!show.copy} data-path={show.copy ? tb.path : null}><span class="icon icon-copy"></span></button>
<button class="dmsg-toolbar-btn btn" data-action="retry" aria-label="Retry send" title="Retry send" hidden={!show.retry}><span class="icon icon-refresh"></span></button>
<button class="dmsg-toolbar-btn btn dmsg-toolbar-btn-danger" data-action="cancel-upload" aria-label="Cancel upload" title="Cancel upload" hidden={!show.cancel}><span class="icon icon-x"></span></button>
<button class="dmsg-toolbar-btn btn dmsg-toolbar-btn-danger" data-action="delete" aria-label={deleteLabel} title={deleteLabel}
        hidden={!show.delete} data-mode={del?.mode || null} data-partial={del?.partial ? '1' : null} data-has-attachments={del?.hasAttachments ? '1' : null}
        style:opacity={del?.partial ? '0.45' : null}><span class="icon icon-trash"></span></button>
