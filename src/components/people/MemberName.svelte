<script>
    // The identity cell of a member-style row: display name plus its marks (rank, bot) on
    // one line. Same DOM as the vanilla buildMemberNameCell so the global styles cascade.
    let {
        display = '',
        hasName = false,          // real name → twemojify; a truncated npub has nothing to render
        rank = null,              // 'owner' | 'admin' | null
        rankLabel = null,         // the role's own name when the shield stands for a positioned role
        bot = false,
        style = '',
        ui,                       // { twemojify, showTooltip, hideTooltip }
    } = $props();

    // Keyed on the string so a re-derive that yields the same name never touches the node
    // (twemojify would restart image loads).
    function name(node, text) {
        let cur;
        const render = (t) => {
            if (t === cur) return;
            cur = t;
            node.textContent = t;
            if (hasName) ui.twemojify(node);
        };
        render(text);
        return { update: render };
    }

    const rankWord = $derived(rank === 'owner' ? 'Owner' : (rankLabel || 'Admin'));
</script>

<div class="member-pick-identity" {style}>
    <div class="member-pick-nameline">
        <div class="compact-member-name" use:name={display}></div>
        {#if rank === 'owner' || rank === 'admin'}
            <span
                class="icon member-rank-mark {rank === 'owner' ? 'icon-crown member-rank-owner' : 'icon-shield-filled member-rank-admin'}"
                role="img"
                aria-label={rankWord}
                onmouseenter={(e) => ui.showTooltip(rankWord, e.currentTarget)}
                onmouseleave={() => ui.hideTooltip()}
            ></span>
        {/if}
        {#if bot}
            <span
                class="icon icon-bot member-pick-bot"
                role="img"
                aria-label="Bot"
                onmouseenter={(e) => ui.showTooltip('Bot', e.currentTarget)}
                onmouseleave={() => ui.hideTooltip()}
            ></span>
        {/if}
    </div>
</div>
