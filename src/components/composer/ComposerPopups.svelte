<script>
    // The composer's three autocomplete panels, body-level like the vanilla ones,
    // each deriving from the shared popup state.
    import MentionPopup from './MentionPopup.svelte';
    import ShortcodePopup from './ShortcodePopup.svelte';
    import CommandPopup from './CommandPopup.svelte';

    let { anchor, h } = $props();
</script>

<MentionPopup {anchor} />
<ShortcodePopup {anchor} {h} />
<CommandPopup {anchor} />
