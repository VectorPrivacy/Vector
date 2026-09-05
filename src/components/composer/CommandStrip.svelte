<script>
    // The "Using /cmd with Bot" strip above the composer. Its content outlives the
    // composer: the strip collapses over it.
    import { composerCommand } from '../lib/composer.svelte.js';

    let { onCancel } = $props();
    const command = composerCommand();
</script>

<span id="chat-command-bar-label">Using <span id="chat-command-bar-cmd">/{command.name}</span> with</span>
<span id="chat-command-bar-bot">
    {#if command.bot?.avatarSrc}<img src={command.bot.avatarSrc} alt="" />{/if}
    <span>{command.bot?.name || ''}</span>
</span>
<span id="chat-command-bar-hint">{command.hint}</span>
<button id="chat-command-bar-cancel" title="Cancel" onclick={onCancel}><span class="icon icon-cancel"></span></button>
