<script>
    // Who is typing in the open chat, in the strip #chat-box keeps reserved between the
    // conversation and the composer: it covers nothing and moves nothing. Faces come and
    // go one by one; the line stays while it fades out.
    import { fly, scale } from 'svelte/transition';
    import { openChatId, chatVersion, profileVersion } from '../lib/signals.svelte.js';
    import { chatHeaderHandlers } from '../lib/chatpane.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    const FACES = 3;

    const typers = $derived.by(() => {
        const id = openChatId();
        const H = chatHeaderHandlers();
        if (!id || !H) return [];
        chatVersion(id);
        const chat = H.getChat(id);
        if (!chat) return [];
        return H.typers(chat).map((npub) => {
            profileVersion(npub);
            const profile = H.getProfile(npub) || null;
            return { npub, name: H.getName(profile || npub), src: H.getProfileAvatarSrc(profile) || null };
        });
    });

    // What the strip shows: the live typers, or the last of them while it fades out.
    // A different chat never inherits the last one's line.
    let shown = $state([]);
    let shownChat = null;
    $effect(() => {
        const id = openChatId();
        const live = typers;
        if (id !== shownChat) { shownChat = id; shown = live; }
        else if (live.length) shown = live;
    });

    const names = $derived(shown.length > 3 ? [] : shown.map((t) => t.name));
    const verb = $derived(shown.length === 1 ? 'is typing' : 'are typing');

    function emojiName(node, name) {
        const render = (n) => { node.textContent = n; chatHeaderHandlers()?.twemojify(node); };
        render(name);
        return { update: render };
    }
</script>

{#if typers.length}
    <div class="typing-strip" role="status" aria-live="polite" transition:fly={{ y: 4, duration: 200 }}>
        <span class="typing-faces">
            {#each shown.slice(0, FACES) as t (t.npub)}
                <span class="typing-face" transition:scale={{ start: 0.3, duration: 180 }}><Avatar src={t.src} size={18} /></span>
            {/each}
        </span>
        <span class="typing-dots" aria-hidden="true"><i></i><i></i><i></i></span>
        <span class="typing-text cutoff">
            {#if !names.length}
                Several people are typing
            {:else}
                {#each names as name, i (i)}
                    {#if i > 0}{i === names.length - 1 ? ' and ' : ', '}{/if}<b use:emojiName={name}></b>
                {/each}
                {verb}
            {/if}
        </span>
    </div>
{/if}
