<script>
    // One person as a row: hover overlay, optional left gutter, avatar, identity, optional
    // trailing control. Every people list in the app (pickers, rosters, banlists) is this
    // row with different snippets in the two slots.
    import Avatar from '../ui/Avatar.svelte';
    import MemberName from './MemberName.svelte';

    let {
        npub,
        profile = null,
        src = null,               // avatar url | null
        display = '',
        hasName = false,
        rank = null,
        rankLabel = null,
        hoverBg = '',             // inline background for the hover overlay ('' = the CSS default)
        dim = false,              // banned / inactive rendering
        acting = false,           // pins hover-revealed controls visible during an async action
        onactivate = null,        // (event) => void — row click / Enter / Space
        gutter = null,            // snippet: the left control slot
        trailing = null,          // snippet: the right control slot
        ui,                       // { placeholder, twemojify, showTooltip, hideTooltip }
    } = $props();

    function key(e) {
        if (!onactivate) return;
        if (e.key !== 'Enter' && e.key !== ' ') return;
        e.preventDefault();
        onactivate(e);
    }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (role is 'button' whenever tabindex is set) -->
<div
    class="member-pick-row"
    class:is-acting={acting}
    data-npub={npub}
    role={onactivate ? 'button' : null}
    tabindex={onactivate ? 0 : null}
    style:cursor={onactivate ? 'pointer' : null}
    onclick={onactivate}
    onkeydown={key}
>
    <div class="member-pick-hover" style="background:{hoverBg}"></div>
    {@render gutter?.()}
    <Avatar {src} size={25} class="member-pick-avatar" style={dim ? 'opacity:0.5;' : ''} placeholder={ui.placeholder} />
    <MemberName
        {display}
        {hasName}
        {rank}
        {rankLabel}
        bot={!!profile?.bot}
        status={profile?.status?.title ? { title: profile.status.title, emojiTags: profile.status.emoji_tags || [] } : null}
        style={dim ? 'opacity:0.6;' : ''}
        {ui}
    />
    {@render trailing?.()}
</div>
