<script>
    // A system event line ("X has joined"), the same DOM insertSystemEvent builds. The
    // name follows the actor's profile signal; a merged repeat hides itself and the run's
    // head carries the count in its suffix.
    import { profileVersion } from '../lib/signals.svelte.js';

    let { msg, merged = false, mergeCount = 1, h } = $props();   // h: RowHelpers (js/render/chat/message-row.js)

    // svelte-ignore state_referenced_locally
    const npub = msg.system_event?.member_npub || null;
    // svelte-ignore state_referenced_locally
    const type = msg.system_event?.event_type ?? null;
    const rich = $derived(!!npub && type !== null);

    const actor = $derived.by(() => {
        if (!npub) return null;
        profileVersion(npub);
        return { name: h.systemEventName(npub), src: h.getProfileAvatarSrc(h.getProfile(npub)) || null };
    });
    const suffix = $derived(h.systemEventSuffix(type) + (mergeCount > 1 ? ` ${mergeCount} times` : ''));

    let avatarFailed = $state(false);
    $effect(() => { actor?.src; avatarFailed = false; });

    function open(e) {
        e.stopPropagation();
        h.showMiniProfile(npub, e.currentTarget.querySelector('.system-event-name'));
    }
</script>

<p
    class="msg-inline-timestamp"
    class:system-event-merged={merged}
    id={msg.id}
    data-at={msg.at}
    data-system-event-type={rich ? String(type) : undefined}
    data-system-event-npub={rich ? npub : undefined}
>
    {#if rich}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions (byte-identical to insertSystemEvent) -->
        <span class="system-event-text" onclick={open}>
            {#if actor?.src && !avatarFailed}
                <!-- svelte-ignore a11y_missing_attribute -->
                <img class="system-event-avatar" src={actor.src} style="width: 16px; height: 16px; object-fit: cover; border-radius: 50%;" onerror={() => (avatarFailed = true)} />
            {:else}
                <!-- svelte-ignore node_invalid_placement_ssr (the vanilla builder places this div the same way) -->
                <div class="placeholder-avatar system-event-avatar" style="min-height: 16px; min-width: 16px; max-height: 16px; max-width: 16px; background-image: url(&quot;icons/user-placeholder.svg&quot;); background-size: cover; background-position: center center;"></div>
            {/if}
            <span class="system-event-name" data-npub={npub}>{actor?.name}</span>
            <span class="system-event-suffix">{suffix}</span>
        </span>
    {:else}
        {msg.content}
    {/if}
</p>
