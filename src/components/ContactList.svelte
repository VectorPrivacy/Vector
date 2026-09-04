<script>
    // Reactive replacement for the vanilla renderContacts + buildContactRow. Renders the
    // filtered/sorted DM-contact picker into #cmt-contacts. Static data + vanilla DOM helpers
    // come in as props, exactly like the chatlist island (h-bundle convention).
    //
    // Dialog-local live state (filter, pasted strangers, selection) lives HERE as $state
    // runes — no bridge stores. The vanilla side drives it through exported instance
    // methods (setFilter / addStranger / select / setProfiles / reset / getSelection, callable
    // on the object mountContactList returns) and listens through the onSelectionChange
    // callback prop. Nothing svelte/store-shaped crosses the boundary.
    let {
        myNpub = '',
        banned = [],
        members = [],
        dmNpubs = [],
        chatTsById = new Map(),
        avatarSrc = () => null,      // (profile) => url | null
        makePlaceholder = () => document.createElement('div'), // () => the default-avatar element
        twemojify = () => {},
        showTooltip = () => {},      // (text, anchorEl) => the app's global tooltip
        hideTooltip = () => {},
        hoverBg = '',                // precomputed plain-gradient string for the row hover overlay
        profiles = [],               // initial snapshot; later profile loads ride setProfiles()
        onSelectionChange = () => {}, // (sel: Set) — fires with a copy on every selection change
    } = $props();

    // Dialog-local reactive state. Raw discipline: profiles are shared by reference with
    // the page's arrProfiles array — never deep-proxy them. The `profiles` prop is an
    // INITIAL SNAPSHOT read once at mount by design; later profile loads ride setProfiles().
    let filter = $state('');
    let strangers = $state.raw([]);        // pasted npubs, rendered only while selected
    let selection = $state.raw(new Set());
    // svelte-ignore state_referenced_locally
    let profilesList = $state.raw(profiles);

    const bannedSet = $derived(new Set(banned));
    const memberSet = $derived(new Set(members));
    const dmSet = $derived(new Set(dmNpubs));
    const profileById = $derived(new Map(profilesList.map((p) => [p.id, p])));

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

    function displayName(p, npub) {
        const nm = p ? p.nickname || p.name || p.display_name || '' : '';
        return nm || npub.slice(0, 10) + '...' + npub.slice(-6);
    }

    // Strangers first (pasted npubs that got selected), then DM contacts: selected first,
    // then most-recent conversation. O(1) chatTsById lookup in the comparator.
    const rows = $derived.by(() => {
        const f = filter.trim().toLowerCase();
        const sel = selection;
        const out = [];
        for (const npub of strangers) {
            if (bannedSet.has(npub) || memberSet.has(npub) || !sel.has(npub)) continue;
            const profile = profileById.get(npub) || null;
            out.push({ npub, profile, src: avatarSrc(profile) });
        }
        const contacts = profilesList
            .filter(
                (p) =>
                    p && p.id && p.id !== myNpub && !p.is_blocked &&
                    !bannedSet.has(p.id) && !memberSet.has(p.id) && dmSet.has(p.id),
            )
            .filter((p) => {
                if (!f) return true;
                const name = p.nickname || p.name || p.display_name || '';
                return (name + ' ' + p.id).toLowerCase().includes(f);
            })
            .sort((a, b) => {
                const aSel = sel.has(a.id), bSel = sel.has(b.id);
                if (aSel !== bSel) return aSel ? -1 : 1;
                return (chatTsById.get(b.id) || 0) - (chatTsById.get(a.id) || 0);
            });
        for (const p of contacts) out.push({ npub: p.id, profile: p, src: avatarSrc(p) });
        return out;
    });

    function toggle(npub) {
        const next = new Set(selection);
        if (next.has(npub)) next.delete(npub);
        else next.add(npub);
        selection = next;
    }

    // The footer CTA morphs with the selection: gray "Done" (dismiss) when nothing's picked,
    // accent "Invite N" (send) once contacts are selected. Fires with a copy so the caller
    // can never mutate the live Set.
    $effect(() => onSelectionChange(new Set(selection)));

    // Reuse the vanilla DOM helpers via actions: the reactive STRUCTURE is Svelte, the leaf
    // avatar/name widgets are the app's own battle-tested builders.
    // Real avatars render as a direct <img> below (matches the vanilla exactly — a direct flex
    // child, so no inline baseline gap and no nested-in-a-div WKWebView re-composite flicker).
    // Only the default-avatar placeholder rides an action (it's a static element, never reloads).
    function placeholderInto(node) {
        node.replaceChildren(makePlaceholder());
    }
    function name(node, text) {
        let cur;
        const render = (t) => {
            if (t === cur) return;
            cur = t;
            node.textContent = t;
            twemojify(node);
        };
        render(text);
        return { update: render };
    }
</script>

{#if rows.length === 0}
    <p class="cmt-empty" style="text-align:center;">
        {filter.trim() ? 'No matches.' : 'No contacts yet. Paste an npub to invite someone.'}
    </p>
{:else}
    {#each rows as row (row.npub)}
        <div
            class="member-pick-row"
            onclick={() => toggle(row.npub)}
            onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), toggle(row.npub))}
            role="button"
            tabindex="0"
        >
            <div class="member-pick-hover" style="background:{hoverBg}"></div>
            {#if row.src}
                <img
                    class="member-pick-avatar"
                    src={row.src}
                    alt=""
                    style="width:25px;height:25px;object-fit:cover;border-radius:50%;"
                />
            {:else}
                <div class="member-pick-avatar" use:placeholderInto></div>
            {/if}
            <div class="member-pick-identity">
                <div class="compact-member-name" use:name={displayName(row.profile, row.npub)}></div>
                {#if row.profile?.bot}
                    <span
                        class="icon icon-bot member-pick-bot"
                        role="img"
                        aria-label="Bot"
                        onmouseenter={(e) => showTooltip('Bot', e.currentTarget)}
                        onmouseleave={() => hideTooltip()}
                    ></span>
                {/if}
            </div>
            <div class="member-pick-indicator" class:selected={selection.has(row.npub)}></div>
        </div>
    {/each}
{/if}

<!-- No <style>: all styling comes from the global styles.css (the .member-pick-* rules cascade
     in), the overlay's plain-gradient inline `hoverBg`, and the avatar's inline sizing. Omitting
     <style> also drops Svelte's scope class, so the rendered DOM is byte-identical to the vanilla
     Create Group row — which is exactly what avoids the WKWebView compositor flicker. -->
