<script>
    // The composer's autocomplete panels and the command parts' choice drop-up,
    // body-level like the vanilla ones, each deriving from shared state.
    import MentionPopup from './MentionPopup.svelte';
    import ShortcodePopup from './ShortcodePopup.svelte';
    import CommandPopup from './CommandPopup.svelte';
    import ChoiceMenu from './ChoiceMenu.svelte';

    let { anchor, h } = $props();
</script>

<MentionPopup {anchor} />
<ShortcodePopup {anchor} {h} />
<CommandPopup {anchor} />
<ChoiceMenu />
