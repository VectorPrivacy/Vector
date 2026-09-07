<script>
    // The expanded profile as ONE reconciler. Renderless: the markup stays in index.html
    // (Edit Mode works on the same elements imperatively), so this adopts them and derives
    // every field from the open profile's signal. The banner and avatar are permanent
    // <img>s; an empty one wears a class instead of being swapped for a div.
    import { profileViewState, profileVersion, chatVersion } from '../lib/signals.svelte.js';
    import { profileEdit, profileEditDirty } from '../lib/profileedit.svelte.js';

    let { els, h } = $props();

    const view = profileViewState();
    const edit = profileEdit();

    const m = $derived.by(() => {
        const id = view.id;
        if (!id) return null;
        profileVersion(id);
        chatVersion(id);
        const p = h.getProfile(id) || { id };
        const hasName = !!(p.nickname || p.name);
        return {
            id, p,
            mine: !!p.mine,
            name: h.getName(p),
            hasName,
            statusTitle: p.status?.title || '',
            emojiTags: p.status?.emoji_tags || [],
            bannerSrc: h.getProfileBannerSrc(p),
            avatarSrc: h.getProfileAvatarSrc(p),
            secondary: p.nickname || p.name || p.display_name || (p.mine ? 'Anonymous' : id.substring(0, 10) + '…'),
            bot: !!p.bot,
            about: typeof p.about === 'string' ? p.about : '',
            muted: !!h.isMuted(id),
            blocked: !!p.is_blocked,
        };
    });
    // Our own profile in Edit Mode: the backend may emit a stale banner mid-edit, and
    // the user's picked preview must survive it.
    const frozen = $derived(!m || (view.editing && m.mine));

    function statusInto(el, t) {
        el.textContent = t.statusTitle || (t.mine ? 'Set a Status' : '');
        if (t.statusTitle) {
            h.twemojify(el);
            h.renderCustomEmojiShortcodes(el, t.emojiTags);
        }
    }

    // ── header ──
    $effect(() => {
        if (frozen) return;
        const t = m;
        els.headerAvatar.replaceChildren();
        if (!t.mine) {
            const img = h.createAvatarImg(t.avatarSrc, 22, false);
            img.classList.add('btn');
            els.headerAvatar.appendChild(img);
        }
        els.headerAvatar.style.display = t.mine ? 'none' : '';
        els.switcher.style.display = t.mine ? '' : 'none';
        els.name.style.display = t.mine ? 'none' : '';
        if (!t.mine) {
            els.name.textContent = t.name;
            if (t.hasName) h.twemojify(els.name);
        }
        statusInto(els.status, t);
        els.status.style.display = t.mine ? 'none' : '';
        const hasStatus = !!els.status.textContent;
        els.name.classList.toggle('chat-contact', !hasStatus);
        els.name.classList.toggle('chat-contact-with-status', hasStatus);
    });

    // ── banner and avatar ──
    $effect(() => {
        if (frozen) return;
        const t = m;
        const b = els.banner;
        const empty = () => { b.removeAttribute('src'); b.classList.add('is-empty'); };
        if (t.bannerSrc) {
            b.classList.remove('is-empty');
            b.onerror = empty;
            b.src = t.bannerSrc;
        } else {
            b.onerror = null;
            empty();
        }
        const dark = t.mine || !!t.p.banner;
        b.style.backgroundColor = dark ? 'rgb(27, 27, 27)' : '';
        b.style.height = dark ? '' : '115px';
    });
    $effect(() => {
        if (frozen) return;
        const t = m;
        const a = els.avatar;
        const placeholder = () => { a.removeAttribute('src'); a.classList.add('is-placeholder'); };
        if (t.avatarSrc) {
            a.classList.remove('is-placeholder');
            a.onerror = placeholder;
            a.src = t.avatarSrc;
        } else {
            a.onerror = null;
            placeholder();
        }
    });

    // ── secondary name and status, description, npub ──
    $effect(() => {
        if (frozen) return;
        const t = m;
        els.secondaryName.textContent = t.secondary;
        if (t.hasName) h.twemojify(els.secondaryName);
        if (t.bot) els.secondaryName.appendChild(h.botIcon());
        statusInto(els.secondaryStatus, t);
        els.description.textContent = t.about || 'No description yet';
        els.description.classList.toggle('group-placeholder', !t.about);
        h.twemojify(els.description);
        if (t.about) h.renderMentions(els.description);
        els.npub.dataset.fullNpub = t.id;
        els.npub.textContent = t.id.slice(0, 16) + '...' + t.id.slice(-16);
        els.npubLabel.textContent = t.mine ? 'My nPub Key' : 'nPub Key';
        els.id.textContent = t.id;
    });

    // ── own profile vs a contact ──
    $effect(() => {
        if (frozen) return;
        const t = m;
        els.root.classList.toggle('is-own-profile', t.mine);
        els.options.style.display = t.mine ? 'none' : '';
        els.editBtn.style.display = t.mine ? 'flex' : 'none';
        els.shareBtn.style.display = t.mine ? 'block' : 'none';
        els.qrBtn.style.display = 'block';
        els.backBtn.style.display = t.mine ? 'none' : '';
        els.navbar.style.display = t.mine ? '' : 'none';
        // Status is the one quick-set field on our own profile; nothing else is click-to-edit.
        // Edit Mode makes the pictures clickable; outside it nothing but the status is.
        for (const el of [els.name, els.secondaryName, els.description, els.avatar, els.banner]) el.classList.remove('btn');
        els.avatar.onclick = null;
        els.banner.onclick = null;
        els.status.classList.toggle('btn', t.mine);
        els.secondaryStatus.classList.toggle('btn', t.mine);
        els.moreDropdown.style.display = 'none';
    });

    // ── contact options ──
    $effect(() => {
        if (frozen) return;
        const t = m;
        const icon = els.optionMute.querySelector('span');
        icon.classList.remove('icon-volume-max', 'icon-volume-mute');
        icon.classList.add(t.muted ? 'icon-volume-mute' : 'icon-volume-max');
        els.optionMute.querySelector('p').innerText = t.muted ? 'Unmute' : 'Mute';
        const blockLabel = els.optionBlock.querySelector('span:first-child');
        if (blockLabel) blockLabel.textContent = t.blocked ? 'Unblock' : 'Block';
        els.optionBlock.classList.add('is-danger');
    });

    // ── badges: async lookups, each guarded against the profile changing under it ──
    $effect(() => {
        if (frozen) return;
        const { id, mine } = m;
        for (const b of [els.badgeInvite, els.badgeFawkes, els.badgeBugHunter]) b.style.display = 'none';
        const live = () => view.id === id;
        h.invitedCount(id).then((count) => {
            if (!live() || !(count > 0)) return;
            els.badgeInvite.style.display = '';
            els.badgeInvite.onclick = () => h.showInviteBadge(count);
        }).catch(() => {});
        h.fawkesBadge(id, mine).then((has) => {
            if (!live() || !has) return;
            els.badgeFawkes.style.display = '';
            els.badgeFawkes.onclick = () => h.showFawkesCard();
        }).catch(() => {});
        h.bugHunterTier(id, mine).then((tier) => {
            if (!live() || !(tier > 0)) return;
            els.badgeBugHunter.src = './icons/bughunter_' + tier + '.svg';
            els.badgeBugHunter.style.display = '';
            els.badgeBugHunter.onclick = () => h.showBugHunterCard(tier);
        }).catch(() => {});
    });

    // ── Edit Mode chrome: the bar, the fields, and everything that steps aside ──
    const stepAside = () => [els.npubLabel, els.npubContainer, els.badges, els.secondaryName, els.secondaryStatus, els.description];
    // The banner buttons are the mode effect's to show again; it runs on the same exit.
    const bannerButtons = () => [els.editBtn, els.shareBtn, els.qrBtn];
    let hideBarTimer = null;
    $effect(() => {
        const on = edit.active;
        clearTimeout(hideBarTimer);
        if (on) {
            els.editBar.style.opacity = '0';
            els.editBar.style.display = 'flex';
            hideBarTimer = setTimeout(() => { els.editBar.style.opacity = '1'; }, 10);
        } else {
            els.editBar.style.opacity = '0';
            hideBarTimer = setTimeout(() => { els.editBar.style.display = 'none'; }, 250);
        }
        els.headerInfo.style.display = on ? 'none' : '';
        if (on) els.backBtn.style.display = 'none';
        for (const el of stepAside()) el.style.display = on ? 'none' : '';
        if (on) for (const el of bannerButtons()) el.style.display = 'none';
        els.editFields.style.display = on ? 'flex' : 'none';
        els.root.classList.toggle('profile-edit-active', on);
        // The pictures are the pick targets while editing.
        for (const el of [els.avatar, els.banner]) el.classList.toggle('btn', on);
        els.avatar.onclick = on ? () => h.pickPicture('avatar') : null;
        els.banner.onclick = on ? () => h.pickPicture('banner') : null;
        if (!on) {
            els.bannerContainer.classList.remove('avatar-hovered');
            return;
        }
        // The banner's overlay yields to the avatar's while the pointer is over the avatar.
        const track = (e) => {
            const bannerRect = els.bannerContainer.getBoundingClientRect();
            const avatarRect = els.avatarContainer.getBoundingClientRect();
            const inBanner = e.clientY <= bannerRect.top + 200;
            const inAvatar = e.clientX >= avatarRect.left && e.clientX <= avatarRect.right
                && e.clientY >= avatarRect.top && e.clientY <= avatarRect.bottom;
            els.bannerContainer.classList.toggle('avatar-hovered', inAvatar || !inBanner);
        };
        els.bannerContainer.addEventListener('mousemove', track);
        return () => els.bannerContainer.removeEventListener('mousemove', track);
    });
    $effect(() => {
        if (!edit.active) return;
        els.editLabel.textContent = profileEditDirty() ? 'Unsaved Changes Made' : 'Edit Mode is Enabled';
        els.editLabel.style.opacity = '0.8';
    });
    // Picked pictures preview in place until save or cancel repaints from the profile.
    $effect(() => {
        if (!edit.active) return;
        if (edit.preview.avatar) { els.avatar.classList.remove('is-placeholder'); els.avatar.src = edit.preview.avatar; }
    });
    $effect(() => {
        if (!edit.active) return;
        if (edit.preview.banner) { els.banner.classList.remove('is-empty'); els.banner.src = edit.preview.banner; }
    });
</script>
