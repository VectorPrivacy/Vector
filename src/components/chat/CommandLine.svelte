<script>
    // The passive invocation line: "JSKitty ran /roll with Concordia". Dim prose, no
    // bubble, the params absent (long values would drown the row; the content still
    // carries them for bots). Names carry data-npub for the shared profile delegate, and
    // the command name is the one-tap rerun the click delegate routes.
    let { msg, cmd, h } = $props();   // cmd: { name, botNpub }; h: myNpub(), getName(x), getProfile(npub), getProfileAvatarSrc(p)
    const authorNpub = $derived(msg.mine ? h.myNpub() : (msg.npub || ''));
    const bot = $derived(cmd.botNpub ? h.getProfile(cmd.botNpub) : null);
    const botAvatar = $derived((bot && h.getProfileAvatarSrc(bot)) || 'icons/user-placeholder.svg');
</script>

<span class="dmsg-text"><span class="dmsg-command-line"><span class="dmsg-command-author" data-npub={authorNpub || undefined}>{h.getName(authorNpub)}</span> ran <span class="dmsg-command-name btn">/{cmd.name}</span>{#if cmd.botNpub}{' with '}<span class="dmsg-command-bot-unit"><img class="dmsg-command-bot-avatar btn" src={botAvatar} alt="" data-npub={cmd.botNpub}><span class="dmsg-command-bot btn" data-npub={cmd.botNpub}>{h.getName(cmd.botNpub)}</span></span>{/if}</span></span>
