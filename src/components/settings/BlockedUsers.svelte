<script>
    // The blocked-users list under Privacy. Re-fetches when its version moves (a
    // block or unblock anywhere), and each row re-derives its name and avatar
    // from the profile signal as profiles resolve.
    import { profileVersion } from '../lib/signals.svelte.js';
    import { blockedVersion } from '../lib/settings.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    let { h } = $props();   // h: load, getProfile, getProfileAvatarSrc, confirmUnblock, unblock, reload

    let users = $state.raw([]);
    $effect(() => {
        blockedVersion();
        h.load().then((list) => { users = list || []; }).catch((e) => console.warn('Failed to load blocked users:', e));
    });

    function resolve(u) {
        profileVersion(u.id);
        const p = h.getProfile(u.id) || u;
        const displayName = p.nickname || p.name || p.display_name;
        return { p, displayName, src: h.getProfileAvatarSrc(p) };
    }

    async function unblock(u, p) {
        const confirmed = await h.confirmUnblock(p);
        if (!confirmed) return;
        await h.unblock(u.id);
        h.reload();
    }
</script>

{#each users as u (u.id)}
    {@const r = resolve(u)}
    <div style="display: flex; align-items: center; justify-content: space-between; padding: 8px 10px;">
        <div style="display: flex; align-items: center; gap: 10px; min-width: 0; flex: 1; -webkit-user-select: none; user-select: none;">
            <Avatar src={r.src} size={30} style="flex-shrink:0;" />
            <span style="color: #ddd; font-size: 14px; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: left;">
                {#if r.displayName}{r.displayName} <span style="opacity: 0.4; font-size: 12px;">({u.id.substring(0, 8)})</span>{:else}{u.id.substring(0, 20)}...{/if}
            </span>
        </div>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="unblock-btn" onclick={() => unblock(u, r.p)}>Unblock</span>
    </div>
{/each}
{#if users.length === 0}
    <p id="settings-blocked-empty" style="color: #666; font-size: 13px;">No blocked users</p>
{/if}
