<script>
    // The contact picker: a filterable, selectable list of the people you have written
    // to. Mounted by the community invite panel (#cmt-contacts) and the Create Group tab
    // (#create-group-list) — one module, two mounts.
    //
    // Dialog-local live state (filter, pasted strangers, selection) lives HERE as $state
    // runes — no bridge stores. The vanilla side drives it through exported instance
    // methods (setFilter / addStranger / select / setProfiles / reset / getSelection) and
    // listens through the onSelectionChange callback prop.
    import MemberRow from './MemberRow.svelte';
    import { profileVersion } from '../lib/signals.svelte.js';

    let {
        myNpub = '',
        banned = [],
        members = [],
        // null = every profile is listable; an array = restrict to those ids (the
        // callers pass the DM contacts).
        dmNpubs = null,
        chatTsById = new Map(),
        avatarSrc = () => null,      // (profile) => url | null
        makePlaceholder = () => document.createElement('div'), // () => the default-avatar element
        twemojify = () => {},
        showTooltip = () => {},      // (text, anchorEl) => the app's global tooltip
        hideTooltip = () => {},
        hoverBg = '',                // precomputed plain-gradient string for the row hover overlay
        profiles = [],               // initial snapshot; later profile loads ride setProfiles()
        getProfile = null,           // (npub) => live profile; rows track its signal when given
        onSelectionChange = () => {}, // (sel: Set) — fires with a copy on every selection change
    } = $props();

    // Raw discipline: profiles are shared by reference with the page's arrProfiles array —
    // never deep-proxy them. The `profiles` prop is an INITIAL SNAPSHOT read once at mount.
    let filter = $state('');
    let strangers = $state.raw([]);        // pasted npubs, rendered only while selected
    let selection = $state.raw(new Set());
    // svelte-ignore state_referenced_locally
    let profilesList = $state.raw(profiles);

    const bannedSet = $derived(new Set(banned));
    const memberSet = $derived(new Set(members));
    const dmSet = $derived(dmNpubs === null ? null : new Set(dmNpubs));
    const profileById = $derived(new Map(profilesList.map((p) => [p.id, p])));
    // The snapshot decides who is LISTED; each row reads the live profile through its
    // signal, so a name, avatar or status change repaints without a re-mount.
    function profileFor(npub, fallback = null) {
        profileVersion(npub);
        return (getProfile ? getProfile(npub) : null) || fallback || profileById.get(npub) || null;
    }

    // svelte-ignore state_referenced_locally
    const ui = { placeholder: makePlaceholder, twemojify, showTooltip, hideTooltip };

    // ── instance exports: the vanilla<->island bridge ──

    /** Typing in the dialog's npub input filters the list. */
    export function setFilter(value) {
        filter = String(value || '');
    }

    /** A profile load landed (stranger's profile fetched) — swap the snapshot. */
    export function setProfiles(value) {
        profilesList = value || [];
    }

    /** Paste-to-invite: a stranger npub enters the list pre-selected. */
    export function addStranger(npub) {
        if (!strangers.includes(npub)) strangers = [...strangers, npub];
        select(npub);
    }

    /** Select from the paste path (no toggle: re-pasting an already-selected npub keeps it). */
    export function select(npub) {
        if (selection.has(npub)) return;
        const next = new Set(selection);
        next.add(npub);
        selection = next;
    }

    /** Selection as of right now (a copy — the caller may mutate it freely). */
    export function getSelection() {
        return new Set(selection);
    }

    /** After a successful send: sweep selection, strangers and filter back to blank. */
    export function reset() {
        selection = new Set();
        strangers = [];
        filter = '';
    }

    function nameOf(p) {
        return p ? p.nickname || p.name || p.display_name || '' : '';
    }
    function displayName(p, npub) {
        return nameOf(p) || npub.slice(0, 10) + '...' + npub.slice(-6);
    }

    // Strangers first (pasted npubs that got selected), then contacts: selected first,
    // then most-recent conversation, then name.
    const rows = $derived.by(() => {
        const f = filter.trim().toLowerCase();
        const sel = selection;
        const out = [];
        const push = (npub, profile) => out.push({ npub, profile, display: displayName(profile, npub), hasName: !!nameOf(profile), src: avatarSrc(profile) });
        for (const npub of strangers) {
            if (bannedSet.has(npub) || memberSet.has(npub) || !sel.has(npub)) continue;
            push(npub, profileFor(npub));
        }
        const contacts = profilesList
            .filter(
                (p) =>
                    p && p.id && p.id !== myNpub && !p.is_blocked &&
                    !bannedSet.has(p.id) && !memberSet.has(p.id) &&
                    (dmSet === null || dmSet.has(p.id)),
            )
            .filter((p) => !f || (nameOf(p) + ' ' + p.id).toLowerCase().includes(f))
            .sort((a, b) => {
                const aSel = sel.has(a.id), bSel = sel.has(b.id);
                if (aSel !== bSel) return aSel ? -1 : 1;
                const d = (chatTsById.get(b.id) || 0) - (chatTsById.get(a.id) || 0);
                if (d) return d;
                return displayName(a, a.id).localeCompare(displayName(b, b.id));
            });
        for (const p of contacts) push(p.id, profileFor(p.id, p));
        return out;
    });

    function toggle(npub) {
        const next = new Set(selection);
        if (next.has(npub)) next.delete(npub);
        else next.add(npub);
        selection = next;
    }

    // Fires with a copy so the caller can never mutate the live Set.
    $effect(() => onSelectionChange(new Set(selection)));
</script>

{#if rows.length === 0}
    <p class="cmt-empty" style="text-align:center;">
        {filter.trim() ? 'No matches.' : 'No contacts yet. Paste an npub to invite someone.'}
    </p>
{:else}
    {#each rows as row (row.npub)}
        <MemberRow
            npub={row.npub}
            profile={row.profile}
            src={row.src}
            display={row.display}
            hasName={row.hasName}
            {hoverBg}
            onactivate={() => toggle(row.npub)}
            {ui}
        >
            {#snippet trailing()}
                <div class="member-pick-indicator" class:selected={selection.has(row.npub)}></div>
            {/snippet}
        </MemberRow>
    {/each}
{/if}

<!-- No <style>: the global .member-pick-* rules cascade in; the DOM matches the vanilla rows. -->
