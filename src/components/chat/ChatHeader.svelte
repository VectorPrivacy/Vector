<script>
    // The open chat's header as ONE reconciler. Renderless: the markup stays in
    // index.html and main.js keeps its cached elements, so this adopts them and
    // derives their content from the open chat and its signals: the peer's profile
    // landing, a typer, a member count or a renamed channel repaints the header
    // with no retro-resolve code.
    import { openChatId, chatVersion, profileVersion, communityVersion } from '../lib/signals.svelte.js';

    let { els, h } = $props();   // els: avatar, name, status, menu

    const vm = $derived.by(() => {
        const id = openChatId();
        if (!id) return null;
        chatVersion(id);
        const chat = h.getChat(id) || null;
        const communityId = chat?.metadata?.custom_fields?.community_id || null;
        if (communityId) communityVersion(communityId);
        else profileVersion(id);
        const notes = id === h.myNpub();
        const group = !notes && !!chat && h.isGroup(chat);
        const profile = notes || group ? null : (h.getProfile(id) || null);
        let name, avatarSrc = null, twemoji = false, click = null;
        if (notes) {
            name = 'Notes';
        } else if (group) {
            name = h.communityChatTitle(chat) || `Group ${id.substring(0, 10)}...`;
            avatarSrc = chat.metadata?.avatar_cached ? h.convertFileSrc(chat.metadata.avatar_cached) : null;
            click = () => h.openCommunity(chat);
        } else {
            name = h.getName(profile || id);
            twemoji = !!(profile?.nickname || profile?.name);
            avatarSrc = h.getProfileAvatarSrc(profile) || null;
            if (profile) click = () => h.openProfile(profile);
        }
        // Subtext: a typer outranks everything but Notes.
        const typing = chat ? h.typingText(chat) : '';
        let subtext = '', tags = [], gradient = false;
        if (notes) subtext = 'Encrypted Notes to Self';
        else if (typing) { subtext = typing; gradient = true; }
        else if (group) subtext = h.memberSubtext(communityId);
        else { subtext = profile?.status?.title || ''; tags = profile?.status?.emoji_tags || []; }
        return { id, notes, group, name, twemoji, avatarSrc, click, subtext, tags, gradient, menu: !!chat && h.menuCount(chat) > 0 };
    });

    // ── avatar ──
    let avatarKey = null;
    $effect(() => {
        const v = vm;
        const key = v && !v.notes ? `${v.id}|${v.avatarSrc}|${v.group}` : '';
        if (key === avatarKey) return;
        avatarKey = key;
        els.avatar.replaceChildren();
        if (!key) return;
        const img = h.createAvatarImg(v.avatarSrc, 22, v.group);
        img.classList.add('btn');
        img.onclick = () => vm?.click?.();
        els.avatar.appendChild(img);
    });

    // ── name ──
    let nameKey = null;
    $effect(() => {
        const v = vm;
        const key = v ? `${v.id}|${v.name}|${v.twemoji}|${!!v.click}` : '';
        if (key === nameKey) return;
        nameKey = key;
        els.name.textContent = v ? v.name : '';
        if (v?.twemoji) h.twemojify(els.name);
        els.name.classList.toggle('btn', !!v?.click);
        els.name.onclick = v?.click ? () => vm?.click?.() : null;
    });

    // ── subtext: status, typing, member count ──
    // Shown text swaps in place; going empty collapses the line (the 300ms wait
    // matches the CSS transition) and a chat switch resets it without the fade.
    let shownId = null;
    let hideTimer = null;
    $effect(() => {
        const v = vm;
        const id = v ? v.id : null;
        if (id !== shownId) {
            shownId = id;
            if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; }
            els.status.textContent = '';
            els.status.classList.add('status-hidden');
            els.status.classList.remove('typing-indicator-text');
            els.name.classList.remove('chat-contact-with-status');
            els.name.classList.add('chat-contact');
        }
        if (!v) return;
        const visible = !!els.status.textContent && !els.status.classList.contains('status-hidden');
        if (v.subtext) {
            if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; }
            els.status.classList.remove('status-hidden');
            els.status.style.display = '';
            els.status.textContent = v.subtext;
            els.status.classList.toggle('typing-indicator-text', v.gradient);
            if (!v.gradient) {
                h.twemojify(els.status);
                h.renderCustomEmojiShortcodes(els.status, v.tags);
            }
            els.name.classList.remove('chat-contact');
            els.name.classList.add('chat-contact-with-status');
        } else if (visible) {
            els.status.classList.add('status-hidden');
            els.name.classList.remove('chat-contact-with-status');
            els.name.classList.add('chat-contact');
            hideTimer = setTimeout(() => {
                els.status.textContent = '';
                els.status.classList.remove('typing-indicator-text');
                hideTimer = null;
            }, 300);
        }
    });

    // ── overflow menu: hidden when the chat has no options ──
    $effect(() => {
        if (els.menu) els.menu.style.display = vm?.menu ? '' : 'none';
    });
</script>
