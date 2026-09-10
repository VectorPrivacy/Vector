<script>
    // The composer's autocomplete panels and the command parts' choice drop-up,
    // body-level like the vanilla ones, each deriving from shared state.
    import MentionPopup from './MentionPopup.svelte';
    import ShortcodePopup from './ShortcodePopup.svelte';
    import CommandPopup from './CommandPopup.svelte';
    import ChoiceMenu from './ChoiceMenu.svelte';

    import { composerEls } from '../lib/composer.svelte.js';
    let { h } = $props();
    // The composer box binds after these mount, so the anchor is read at measure time.
    const anchor = () => composerEls().box;
</script>

<MentionPopup {anchor} />
<ShortcodePopup {anchor} {h} />
<CommandPopup {anchor} />
<ChoiceMenu />
