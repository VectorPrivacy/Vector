<script>
    // The community's identity at the top of its channel pane (widescreen): icon,
    // name, member count, raid pip. Derives from the pane's community and that
    // community's signal, so a count landing or a rename repaints one span.
    import { untrack } from 'svelte';
    import { paneState, communityVersion } from '../lib/signals.svelte.js';

    let { h } = $props();

    const vm = $derived.by(() => {
        const id = paneState().communityId;
        if (!id) return null;
        communityVersion(id);
        const primary = h.primaryChat(id) || null;
        const cf = primary?.metadata?.custom_fields || {};
        return {
            id,
            primary,
            name: cf.name || 'Community',
            avatarSrc: primary?.metadata?.avatar_cached ? h.convertFileSrc(primary.metadata.avatar_cached) : null,
            members: h.communityMemberSubtext(id),
            raid: h.raidAlert(id),
            v2: cf.proto_version === '2',
        };
    });

    // The count and the raid verdict are fetched when the pane shows a community;
    // their landing touches the community. Keyed on the id alone: their own
    // landing must not refetch.
    $effect(() => {
        const id = vm?.id;
        const v2 = !!vm?.v2;
        if (!id) return;
        untrack(() => {
            h.refreshMemberCount(id);
            if (v2) h.refreshRaidAlert(id);
        });
    });

    // The avatar is the head's first child, built by the app's avatar helpers.
    function avatarInto(node, src) {
        let cur = null;
        let el = null;
        const render = (s) => {
            if (el && s === cur) return;
            cur = s;
            if (el) el.remove();
            el = s ? h.createAvatarImg(s, 36, true) : h.createPlaceholderAvatar(true, 36);
            el.classList.add('chatlist-community-head-avatar');
            node.insertBefore(el, node.firstChild);
        };
        render(src);
        return { update: render };
    }

    function name(node, text) {
        let cur;
        const render = (t) => {
            if (t === cur) return;
            cur = t;
            node.textContent = t;
            h.twemojify(node);
        };
        render(text);
        return { update: render };
    }
</script>

{#if vm}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions (byte-identical to the vanilla head) -->
    <div
        class="chatlist-community-head btn"
        id="chatlist-community-head"
        use:avatarInto={vm.avatarSrc}
        onclick={(e) => h.openCommunityMenu(vm.primary, e)}
    >
        <div class="chatlist-community-head-meta">
            <span class="chatlist-community-head-name cutoff" use:name={vm.name}></span>
            <span class="chatlist-community-head-members">
                <!-- The glyph says "people" before the number is read, and holds the
                     line's height while the count is still empty. -->
                <span class="icon icon-users-multi chatlist-community-head-members-icon"></span>
                <span>{vm.members}</span>
            </span>
        </div>
        {#if vm.raid}
            <span class="chatlist-community-head-alert" title="{vm.raid.suspects} accounts flagged as a raid — open Moderation"></span>
        {/if}
        <!-- `.icon` fills its parent, so the caret needs a box of its own. -->
        <div class="chatlist-community-head-caret"><span class="icon icon-chevron-down"></span></div>
    </div>
{/if}
