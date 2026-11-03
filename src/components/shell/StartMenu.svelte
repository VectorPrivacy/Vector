<script>
    // The widescreen Start Menu: search, the app's own places down the left, the pins
    // and their folders on the right, and you (with your audio devices) along the
    // bottom. You ARE the button: the menu grows out of your row in the rail and shrinks
    // back into it, its footer standing where the row stood.
    import { tick } from 'svelte';
    import { flip } from 'svelte/animate';
    import { cubicOut } from 'svelte/easing';
    import { reorderable, nearestByCentre } from '../lib/reorder.js';
    import { startMenuState, setStartOpen, setStartVisible } from '../lib/startmenu.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    import { accountState } from '../lib/account.svelte.js';
    import { micState } from '../lib/calls.svelte.js';
    import { mktIcons } from '../lib/marketplace.svelte.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';
    import AppIcon from '../marketplace/AppIcon.svelte';

    let { h } = $props();   // h: StartMenuHelpers (js/startmenu.js)

    const st = startMenuState();
    const shell = shellState();
    const account = accountState();

    let query = $state('');
    let adding = $state(false);
    let folderId = $state(null);
    let selected = $state(0);
    let searchEl = $state(null);
    let menuEl = $state(null);
    let footerAccount = $state(null);
    let footerAvatar = $state(null);

    const shown = $derived(st.open && st.mode === 'start' && shell.ws);
    // Mounted from the moment it opens until it has shrunk away.
    let rendered = $state(false);
    let closing = false;
    $effect(() => {
        if (shown) {
            if (!rendered) {
                rendered = true;
                setStartVisible(true);
                h.prepare();
                tick().then(() => { grow(); searchEl?.focus(); });
            } else if (closing) {
                closing = false;
                for (const a of menuEl?.getAnimations({ subtree: true }) || []) a.cancel();
                grow();
            }
            return;
        }
        cancelDrag();
        query = '';
        adding = false;
        folderId = null;
        if (rendered && !closing) shrink();
    });

    // The row's box, and its avatar's, as the menu is: where it grows from and shrinks to.
    function morph() {
        const m = menuEl.getBoundingClientRect();
        const row = st.anchor?.row;
        const rowAvatar = st.anchor?.avatar;
        if (!row || !rowAvatar || !footerAvatar) return null;
        const clipFrom = `inset(${Math.max(0, row.top - m.top)}px ${Math.max(0, m.right - (row.left + row.width))}px 0px 0px round 12px)`;
        const block = footerAccount.getBoundingClientRect();
        const av = footerAvatar.getBoundingClientRect();
        const scale = rowAvatar.width / av.width;
        const dx = rowAvatar.left - block.left - (av.left - block.left) * scale;
        const dy = rowAvatar.top - block.top - (av.top - block.top) * scale;
        return { clipFrom, clipTo: 'inset(0px 0px 0px 0px round 24px)', me: `translate(${dx}px, ${dy}px) scale(${scale})` };
    }

    const GROW = { duration: 280, easing: 'cubic-bezier(0.2, 0.9, 0.25, 1)' };
    const SHRINK = { duration: 200, easing: 'cubic-bezier(0.4, 0, 0.7, 0.2)', fill: 'forwards' };

    function grow() {
        const f = menuEl && morph();
        if (!f) return;
        menuEl.animate([{ clipPath: f.clipFrom }, { clipPath: f.clipTo }], GROW);
        footerAccount.animate([{ transform: f.me }, { transform: 'none' }], GROW);
        for (const el of menuEl.querySelectorAll('.stm-fade')) {
            el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200, delay: 80, easing: 'ease-out', fill: 'backwards' });
        }
    }

    function shrink() {
        const f = menuEl && morph();
        const done = () => {
            if (!closing) return;
            closing = false;
            rendered = false;
            setStartVisible(false);
        };
        closing = true;
        if (!f) return done();
        const a = menuEl.animate([{ clipPath: f.clipTo }, { clipPath: f.clipFrom }], SHRINK);
        footerAccount.animate([{ transform: 'none' }, { transform: f.me }], SHRINK);
        for (const el of menuEl.querySelectorAll('.stm-fade')) {
            el.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 120, easing: 'ease-in', fill: 'forwards' });
        }
        a.finished.then(done, done);
    }

    // What each key shows is the device's own answer, asked again whenever anything moves.
    const resolve = (key) => { st.seq; mktIcons(); return h.resolve(key); };
    const nodes = $derived.by(() => { st.seq; return st.layout.nodes || []; });
    const folder = $derived(folderId ? nodes.find((n) => n.type === 'folder' && n.id === folderId) || null : null);
    $effect(() => { if (folderId && !folder) folderId = null; });

    const searching = $derived(!!query.trim() || adding);
    const results = $derived.by(() => {
        st.seq;
        const list = searching ? h.search(query.trim(), { adding }) : [];
        for (const r of list) if (r.person) profileVersion(r.person);
        return list;
    });
    $effect(() => { results; selected = 0; });

    const nav = $derived.by(() => { st.seq; shell.tab; shell.invitesTab; return h.navItems(); });
    const mic = micState();

    function close() { setStartOpen(false); }
    function run(fn) { close(); fn(); }

    function openKey(key) { run(() => h.open(key)); }

    function onKey(e) {
        if (!shown) return;
        if (e.key === 'Escape') {
            e.preventDefault();
            if (query) query = '';
            else if (adding) adding = false;
            else if (folderId) folderId = null;
            else close();
            return;
        }
        if (!searching || !results.length) return;
        if (e.key === 'ArrowDown') { e.preventDefault(); selected = (selected + 1) % results.length; }
        else if (e.key === 'ArrowUp') { e.preventDefault(); selected = (selected - 1 + results.length) % results.length; }
        else if (e.key === 'Enter') {
            e.preventDefault();
            const r = results[selected];
            if (adding && r.key) h.togglePin(r.key);
            else if (r) run(r.run);
        }
    }

    // A press outside the menu (and outside what opened it) puts it away.
    function onPointerDown(e) {
        if (!shown || dragId !== null) return;
        if (menuEl?.contains(e.target) || e.target.closest?.('#ws-rail-account, .context-menu')) return;
        close();
    }

    // Its bottom-left corner is the row's, so the footer stands where the row stood.
    const place = $derived.by(() => {
        const row = st.anchor?.row;
        const bottom = row ? Math.max(0, window.innerHeight - (row.top + row.height)) : 12;
        // One size, whatever the pane shows: the list scrolls, the menu holds still.
        return { left: row ? row.left : 12, bottom, height: Math.min(560, window.innerHeight - bottom - 12) };
    });

    // ── Dragging pins about: onto a pin makes a folder, onto a folder files it, beside a
    //    pin places it there. The rails' press arbiter decides drag, click or menu.
    let dragId = $state(null);   // the lifted node: 'i:<key>' or 'f:<folder id>'
    let armedId = $state(null);
    let dropAt = $state(null);   // { id, mode: 'into' | 'before' | 'after' | 'end' }
    let ghost = null;            // the lifted icon riding the pointer: { el, offX, offY }
    let pane = $state(null);
    const SLIDE = { duration: 220, easing: cubicOut };

    const sourceId = (s) => (s.kind === 'folder' ? `f:${s.id}` : `i:${s.key}`);
    const nodeIdOf = (n) => (n.type === 'folder' ? `f:${n.id}` : `i:${n.key}`);

    function gestures(source, node) {
        const id = sourceId(source);
        return {
            onMenu: (x, y) => h.tileMenu(x, y, node, folder?.id || null, (fid) => { folderId = fid; }, (fid) => { folderId = fid; renaming = true; }),
            closeMenu: () => h.closeMenu(),
            onArm: (on) => { armedId = on ? id : (armedId === id ? null : armedId); },
            onDragStart: (ev, el) => { dragId = id; ghost = lift(el, ev); },
            onDragMove: (mv) => {
                ghost?.el.style.setProperty('left', `${mv.clientX - ghost.offX}px`);
                ghost?.el.style.setProperty('top', `${mv.clientY - ghost.offY}px`);
                dropAt = resolveDrop(mv.clientX, mv.clientY, id);
                autoScroll(mv.clientY);
            },
            onDragEnd: (up) => land(source, resolveDrop(up.clientX, up.clientY, id)),
        };
    }

    // The icon alone lifts, held where it was grabbed.
    function lift(el, ev) {
        const box = el.querySelector('.stm-box');
        const r = box.getBoundingClientRect();
        const g = box.cloneNode(true);
        g.classList.add('stm-ghost');
        Object.assign(g.style, { position: 'fixed', width: `${r.width}px`, height: `${r.height}px`, left: `${r.left}px`, top: `${r.top}px` });
        document.body.appendChild(g);
        return { el: g, offX: ev.clientX - r.left, offY: ev.clientY - r.top };
    }

    // The nearest tile always answers, so the marker never drops out over a gap. Its icon's
    // middle files into it; either side places beside it.
    function resolveDrop(x, y, id) {
        if (!grid) return null;
        const tiles = [...grid.querySelectorAll('[data-node-id]')].map((el) => ({ key: el.dataset.nodeId, el }));
        const hit = nearestByCentre(tiles, x, y);
        if (!hit || hit.key === id) return null;
        if (hit.key === 'add') return { id: 'add', mode: 'end' };
        const b = hit.el.querySelector('.stm-box').getBoundingClientRect();
        const inBox = x > b.left + b.width * 0.22 && x < b.right - b.width * 0.22 && y > b.top && y < b.bottom;
        if (inBox && id.startsWith('i:') && !folder) return { id: hit.key, mode: 'into' };
        return { id: hit.key, mode: hit.before ? 'before' : 'after' };
    }

    // Auto-scroll while the drag sits at the pane's top or bottom edge, faster the further past.
    let autoRaf = null;
    let dragY = 0;
    function autoTick() {
        autoRaf = null;
        if (!pane || dragId === null) return;
        const r = pane.getBoundingClientRect();
        const zone = 28;
        const v = dragY < r.top + zone ? -Math.min(12, (r.top + zone - dragY) * 0.3)
            : dragY > r.bottom - zone ? Math.min(12, (dragY - (r.bottom - zone)) * 0.3) : 0;
        if (!v) return;
        const was = pane.scrollTop;
        pane.scrollTop = was + v;
        if (pane.scrollTop !== was) autoRaf = requestAnimationFrame(autoTick);
    }
    function autoScroll(y) { dragY = y; if (!autoRaf) autoRaf = requestAnimationFrame(autoTick); }

    // Release: the layout lands, then the icon glides into its new place, or into the
    // folder it joined, while the other tiles slide to theirs.
    async function land(source, at) {
        if (dragId === null) return;
        if (autoRaf) { cancelAnimationFrame(autoRaf); autoRaf = null; }
        const g = ghost?.el;
        ghost = null;
        const id = dragId;
        dropAt = null;
        // A pin joined with another becomes a folder in that pin's place.
        const into = at?.mode === 'into' ? tileEl(at.id) : null;
        const intoSlot = into ? into.querySelector('.stm-box').getBoundingClientRect() : null;
        if (at) await h.drop(source, target(at, source), folder?.id || null).catch(() => {});
        dragId = null;
        await tick();
        if (!g) return;
        const home = into ? null : tileEl(id);
        const to = intoSlot || (home ? slotOf(home) : null);
        if (!to) { g.remove(); return; }
        const from = g.getBoundingClientRect();
        const box = home?.querySelector('.stm-box');
        if (box) box.style.visibility = 'hidden';
        const scale = into ? 0.4 : 1;
        const dx = to.left + (to.width - from.width) / 2 - from.left;
        const dy = to.top + (to.height - from.height) / 2 - from.top;
        const a = g.animate([
            { transform: getComputedStyle(g).transform, opacity: 1 },
            { transform: `translate(${dx}px, ${dy}px) scale(${scale})`, opacity: into ? 0 : 1 },
        ], { duration: 220, easing: 'cubic-bezier(0.2, 0.8, 0.2, 1)', fill: 'forwards' });
        // A hidden window never finishes an animation, and the icon must not stay lifted.
        const done = () => { g.remove(); if (box) box.style.visibility = ''; };
        a.finished.then(done, done);
        setTimeout(done, 400);
    }

    const tileEl = (id) => grid?.querySelector(`[data-node-id="${CSS.escape(id)}"]`);

    // Where a tile's icon rests once its slide is done: layout offsets ignore the slide's transform.
    function slotOf(tile) {
        const g = grid.getBoundingClientRect();
        const box = tile.querySelector('.stm-box');
        return { left: g.left + tile.offsetLeft + box.offsetLeft, top: g.top + tile.offsetTop + box.offsetTop, width: box.offsetWidth, height: box.offsetHeight };
    }

    /** Drop a drag in flight without moving anything. */
    function cancelDrag() {
        if (autoRaf) { cancelAnimationFrame(autoRaf); autoRaf = null; }
        ghost?.el.remove();
        ghost = null;
        dragId = null;
        dropAt = null;
    }

    // Where a release lands, as the rail's drop grammar says it. "After" is "before the
    // next one", counted without the dragged pin, which leaves its place as it moves.
    function target(at, source) {
        if (at.mode === 'end') return folder ? { at: 'into-folder', folder_id: folder.id } : { at: 'end' };
        const ref = at.id.slice(2);
        if (folder) {
            if (at.mode === 'after') {
                const keys = folder.keys.filter((k) => k !== source.key);
                const next = keys[keys.indexOf(ref) + 1];
                return { at: 'into-folder', folder_id: folder.id, ...(next ? { before_key: next } : {}) };
            }
            return { at: 'into-folder', folder_id: folder.id, before_key: ref };
        }
        if (at.mode === 'into') return at.id.startsWith('f:') ? { at: 'into-folder', folder_id: ref } : { at: 'combine', with_key: ref };
        const others = nodes.filter((n) => nodeIdOf(n) !== sourceId(source));
        const index = others.findIndex((n) => nodeIdOf(n) === at.id);
        const anchorNode = at.mode === 'after' ? others[index + 1] : others[index];
        if (!anchorNode) return { at: 'end' };
        return { at: 'before', anchor: anchorNode.type === 'folder' ? { kind: 'folder', id: anchorNode.id } : { kind: 'item', key: anchorNode.key } };
    }

    let grid = $state(null);

    // Folder names are edited where they show.
    let renaming = $state(false);
    // Taken, not autofocus: autofocus yields to whatever holds focus, and the search box always does.
    function takeFocus(el) {
        requestAnimationFrame(() => { el.focus(); el.select(); });
    }
    function rename(e) {
        renaming = false;
        const name = e.currentTarget.value.trim();
        if (folder && name !== folder.name) h.renameFolder(folder.id, name);
    }
</script>

<svelte:window onkeydown={onKey} onpointerdown={onPointerDown} />

{#snippet glyph(r, size)}
    {#if r.app}
        <AppIcon app={r.app} h={h.mkt()} size={size} />
    {:else if r.src}
        <img class="stm-img" class:round={r.round} style:width="{size}px" style:height="{size}px" src={r.src} alt="" draggable="false">
    {:else if r.initial}
        <span class="stm-initial" class:round={r.round} style:width="{size}px" style:height="{size}px">{r.initial}</span>
    {:else}
        <span class="stm-glyph" style:width="{size}px" style:height="{size}px"><span class="icon icon-{r.icon || 'share'}"></span></span>
    {/if}
{/snippet}

{#snippet tile(node)}
    {#if node.type === 'folder'}
        {@const members = (node.keys || []).slice(0, 4).map(resolve)}
        {@const id = `f:${node.id}`}
        <button type="button" class="stm-tile" data-node-id={id}
                class:is-armed={armedId === id} class:is-dragging={dragId === id}
                class:drop-into={dropAt?.id === id && dropAt.mode === 'into'}
                class:drop-before={dropAt?.id === id && dropAt.mode === 'before'}
                class:drop-after={dropAt?.id === id && dropAt.mode === 'after'}
                use:reorderable={gestures({ kind: 'folder', id: node.id }, node)}
                onclick={() => { folderId = node.id; }}>
            <span class="stm-box stm-folder">
                {#each members as m, i (i)}<span class="stm-mini">{@render glyph(m, 22)}</span>{/each}
            </span>
            <span class="stm-label">{node.name || 'Folder'}</span>
        </button>
    {:else}
        {@const r = resolve(node.key)}
        {@const id = `i:${node.key}`}
        <button type="button" class="stm-tile" data-node-id={id} title={r.label}
                class:missing={r.missing} class:is-armed={armedId === id} class:is-dragging={dragId === id}
                class:drop-into={dropAt?.id === id && dropAt.mode === 'into'}
                class:drop-before={dropAt?.id === id && dropAt.mode === 'before'}
                class:drop-after={dropAt?.id === id && dropAt.mode === 'after'}
                use:reorderable={gestures({ kind: 'item', key: node.key }, node)}
                onclick={() => openKey(node.key)}>
            <span class="stm-box" class:is-app={!!r.app} class:is-bare={!!(r.src || r.initial)}>{@render glyph(r, r.src || r.initial ? 80 : 40)}</span>
            <span class="stm-label">{r.label}</span>
        </button>
    {/if}
{/snippet}

{#if rendered}
    <div class="stm" bind:this={menuEl} role="dialog" aria-label="Start"
         style:left="{place.left}px" style:bottom="{place.bottom}px" style:height="{place.height}px">
        <div class="stm-search stm-fade">
            <svg width="18" height="18" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                <path d="M10.672 10.672L8.27418 8.27418M9.56955 5.15978C9.56955 7.59524 7.59524 9.56955 5.15978 9.56955C2.72432 9.56955 0.75 7.59524 0.75 5.15978C0.75 2.72432 2.72432 0.75 5.15978 0.75C7.59524 0.75 9.56955 2.72432 9.56955 5.15978Z" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" stroke="#B2B2B2"/>
            </svg>
            <input bind:this={searchEl} bind:value={query} placeholder={adding ? 'Find something to pin' : 'Search'}
                   autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false">
        </div>

        <div class="stm-body stm-fade">
            <nav class="stm-nav">
                {#each nav as item (item.id)}
                    <button type="button" class="stm-nav-item" class:active={item.active}
                            onclick={() => run(item.run)} oncontextmenu={(e) => { e.preventDefault(); h.navMenu(e.clientX, e.clientY, item); }}>
                        <span class="icon icon-{item.icon}"></span>
                        <span class="stm-nav-label">{item.label}</span>
                        {#if item.dot}<span class="stm-dot"></span>{/if}
                    </button>
                {/each}
            </nav>

            <section class="stm-pane" bind:this={pane}>
                {#if searching}
                    <div class="stm-head" class:hidden={!adding && !results.length}>
                        {#if adding}
                            <button type="button" class="stm-back" aria-label="Back" onclick={() => { adding = false; query = ''; }}><span class="icon icon-chevron-left"></span></button>
                        {/if}
                        <span>{adding ? 'Add Pins' : 'Results'}</span>
                    </div>
                    <div class="stm-results">
                        {#each results as r, i (r.id)}
                            <!-- svelte-ignore a11y_click_events_have_key_events -->
                            <div class="stm-result" class:selected={i === selected} role="option" aria-selected={i === selected} tabindex="-1"
                                 onpointerenter={() => { selected = i; }} onclick={() => (adding && r.key ? h.togglePin(r.key) : run(r.run))}>
                                {@render glyph(r, 28)}
                                <span class="stm-result-text">
                                    <span class="stm-result-label">{r.label}</span>
                                    {#if r.sub}<span class="stm-result-sub">{r.sub}</span>{/if}
                                </span>
                                {#if r.message && !adding}
                                    <button type="button" class="stm-act" title="Message"
                                            onclick={(e) => { e.stopPropagation(); run(r.message); }}>
                                        <span class="icon icon-message"></span>
                                    </button>
                                {/if}
                                {#if r.key}
                                    <button type="button" class="stm-act stm-pin" class:on={r.pinned} title={r.pinned ? 'Unpin from Start' : 'Pin to Start'}
                                            onclick={(e) => { e.stopPropagation(); h.togglePin(r.key); }}>
                                        <span class="icon icon-pin"></span>
                                    </button>
                                {/if}
                            </div>
                        {:else}
                            <div class="stm-none">
                                <span class="stm-none-glyph" aria-hidden="true">
                                    <svg width="26" height="26" viewBox="0 0 12 12" fill="none">
                                        <path d="M10.672 10.672L8.27418 8.27418M9.56955 5.15978C9.56955 7.59524 7.59524 9.56955 5.15978 9.56955C2.72432 9.56955 0.75 7.59524 0.75 5.15978C0.75 2.72432 2.72432 0.75 5.15978 0.75C7.59524 0.75 9.56955 2.72432 9.56955 5.15978Z" stroke-width="1" stroke-linecap="round" stroke-linejoin="round" stroke="currentColor"/>
                                    </svg>
                                </span>
                                <p class="stm-none-title">
                                    {adding ? 'Nothing to pin called' : 'No results for'} <span class="stm-none-query">“{query.trim()}”</span>
                                </p>
                                <p class="stm-none-sub">{adding ? 'Pin a Mini App, a chat, a community or a place in Settings.' : 'Try a name, a community, a Mini App or a setting.'}</p>
                                {#if !adding && query.trim()}
                                    <button type="button" class="stm-none-action" onclick={() => run(() => h.searchNexus(query.trim()))}>
                                        <span class="icon icon-nexus"></span>Search the Nexus
                                    </button>
                                {/if}
                            </div>
                        {/each}
                    </div>
                {:else if folder}
                    <div class="stm-head">
                        <button type="button" class="stm-back" aria-label="Back" onclick={() => { folderId = null; }}><span class="icon icon-chevron-left"></span></button>
                        {#if renaming}
                            <input class="stm-rename" value={folder.name} maxlength="48" use:takeFocus
                                   onblur={rename} onkeydown={(e) => { if (e.key === 'Enter') e.currentTarget.blur(); if (e.key === 'Escape') { e.stopPropagation(); e.currentTarget.value = folder.name; renaming = false; } }}>
                        {:else}
                            <button type="button" class="stm-folder-name" title="Rename" onclick={() => { renaming = true; }}>{folder.name || 'Folder'}</button>
                        {/if}
                    </div>
                    <div class="stm-grid" bind:this={grid}>
                        {#each folder.keys as key (key)}<div class="stm-cell" animate:flip={SLIDE}>{@render tile({ type: 'item', key })}</div>{/each}
                    </div>
                {:else}
                    <div class="stm-head"><span>Pinned</span></div>
                    <div class="stm-grid" bind:this={grid}>
                        {#each nodes as node (nodeIdOf(node))}<div class="stm-cell" animate:flip={SLIDE}>{@render tile(node)}</div>{/each}
                        <button type="button" class="stm-tile" data-node-id="add" class:drop-before={dropAt?.mode === 'end'}
                                onclick={() => { adding = true; query = ''; tick().then(() => searchEl?.focus()); }}>
                            <span class="stm-box stm-add"><span class="icon icon-plus-circle"></span></span>
                            <span class="stm-label">Add Pin</span>
                        </button>
                    </div>
                {/if}
            </section>
        </div>

        <footer class="stm-foot">
            <!-- Your row, grown: pressing it again folds the menu back into it. -->
            <div class="stm-me" bind:this={footerAccount}>
                <button type="button" class="stm-me-main" title="Close" onclick={close}>
                    <span class="stm-me-avatar" bind:this={footerAvatar}><Avatar src={account.avatarSrc} size={48} /></span>
                    <span class="stm-me-text">
                        <span class="stm-me-name cutoff">{account.name || 'You'}</span>
                        <span class="stm-me-npub cutoff">{h.myNpubShort()}</span>
                    </span>
                    <span class="stm-caret" aria-hidden="true"></span>
                </button>
            </div>
            <span class="stm-foot-gap"></span>
            <span class="stm-audio stm-fade">
                <button type="button" class="stm-audio-btn" class:off={mic.deafened}
                        title={mic.deafened ? 'Undeafen' : 'Deafen'} onclick={() => h.toggleDeafen()}>
                    <span class="icon icon-{mic.deafened ? 'headphones-off' : 'headphones'}"></span>
                </button>
                <button type="button" class="stm-audio-more" title="Output device" aria-label="Output device"
                        onclick={(e) => h.deviceMenu('output', e.currentTarget)}>
                    <span class="icon icon-chevron-down"></span>
                </button>
            </span>
            <span class="stm-audio stm-fade">
                <button type="button" class="stm-audio-btn" class:off={mic.muted || mic.deafened}
                        title={mic.muted || mic.deafened ? 'Unmute' : 'Mute'} onclick={() => h.toggleMute()}>
                    <span class="icon icon-{mic.muted || mic.deafened ? 'mic-off' : 'mic-on'}"></span>
                </button>
                <button type="button" class="stm-audio-more" title="Input device" aria-label="Input device"
                        onclick={(e) => h.deviceMenu('input', e.currentTarget)}>
                    <span class="icon icon-chevron-down"></span>
                </button>
            </span>
        </footer>
    </div>
{/if}

<style>
    .stm {
        position: fixed;
        z-index: 9000;
        width: 680px;
        max-width: calc(100vw - 24px);
        display: flex;
        flex-direction: column;
        border: 1px solid #333;
        border-radius: 24px;
        background-color: #1a1a1a;
        box-shadow: 0 24px 80px rgba(0, 0, 0, 0.6);
        overflow: hidden;
        transform-origin: bottom left;
        color: #fff;
        -webkit-user-select: none;
        user-select: none;
    }
    .stm-search {
        display: flex;
        align-items: center;
        gap: 12px;
        margin: 24px 28px 16px;
        padding: 0 18px;
        height: 44px;
        border-radius: 22px;
        background-color: #0f0f0f;
    }
    .stm-search svg { flex: none; }
    .stm-search input {
        all: unset;
        flex: 1;
        min-width: 0;
        font-family: 'Rubik', Inter, Avenir, Helvetica, Arial, sans-serif;
        font-size: 15px;
        font-weight: 400;
        line-height: 1;
        letter-spacing: 0;
        color: #fff;
        -webkit-user-select: text;
        user-select: text;
    }
    .stm-search input::placeholder { color: #5B6060; }

    .stm-body { display: flex; min-height: 0; flex: 1; padding: 6px 0 22px; }
    .stm-nav {
        display: flex;
        flex-direction: column;
        gap: 4px;
        width: 200px;
        flex: none;
        padding: 6px 14px 6px 18px;
        border-right: 1px solid #333;
    }
    .stm-nav-item {
        all: unset;
        position: relative;
        display: flex;
        align-items: center;
        gap: 14px;
        padding: 9px 12px;
        border-radius: 10px;
        font-size: 15px;
        color: #e6e6e6;
        cursor: pointer;
    }
    .stm-nav-item .icon { position: static; flex: none; width: 24px; height: 24px; margin: 0; background-color: #9a9a9a; }
    .stm-nav-item.active { background-color: #2a2a2a; }
    .stm-nav-item.active .icon { background-color: var(--icon-color-primary); }
    .stm-nav-item:focus-visible { outline: 2px solid var(--icon-color-primary); }
    .stm-dot { position: absolute; left: 30px; top: 8px; width: 8px; height: 8px; border-radius: 50%; background-color: var(--icon-color-primary); }

    /* Always scrollable: the scrollbar's room is kept (its track is transparent), so
       nothing shifts as a list grows past the pane. */
    .stm-pane { flex: 1; min-width: 0; display: flex; flex-direction: column; padding: 0 14px 0 24px; overflow-y: scroll; }
    /* A long list scrolls; it never squeezes the header above it. */
    .stm-pane > * { flex-shrink: 0; }
    .stm-head {
        display: flex;
        align-items: center;
        gap: 6px;
        height: 32px;
        margin-bottom: 10px;
        font-size: 12px;
        font-weight: 600;
        letter-spacing: 0.16em;
        text-transform: uppercase;
        color: rgba(255, 255, 255, 0.5);
    }
    .stm-back { all: unset; display: flex; padding: 4px; border-radius: 8px; cursor: pointer; }
    .stm-back .icon { position: static; width: 18px; height: 18px; margin: 0; background-color: #bbb; }
    .stm-back:hover { background-color: #2a2a2a; }
    .stm-folder-name { all: unset; font-size: 15px; font-weight: 700; letter-spacing: 0; text-transform: none; color: #fff; cursor: text; }
    .stm-rename {
        all: unset;
        flex: 1;
        font-size: 15px;
        font-weight: 700;
        letter-spacing: 0;
        text-transform: none;
        color: #fff;
        border-bottom: 1px solid var(--icon-color-primary);
        -webkit-user-select: text;
        user-select: text;
    }

    .stm-grid { position: relative; display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 16px 14px; }
    .stm-cell { display: grid; min-width: 0; }
    .stm-tile {
        all: unset;
        position: relative;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 8px;
        min-width: 0;
        cursor: pointer;
        touch-action: none;
    }
    .stm-box {
        position: relative;
        display: flex;
        align-items: center;
        justify-content: center;
        width: 100%;
        max-width: 80px;
        aspect-ratio: 1;
        box-sizing: border-box;
        border: 1.5px solid #3a3a3a;
        border-radius: 12px;
        background-color: #222;
        transition: border-color 0.12s ease, background-color 0.12s ease, transform 0.16s ease, opacity 0.16s ease, box-shadow 0.16s ease;
    }
    /* A Mini App's icon is the whole tile, and a contact's or community's picture stands on no plate. */
    .stm-box.is-app, .stm-box.is-bare { border: 0; background-color: transparent; }
    .stm-box.is-app :global(.nx-icon) { width: 100%; height: 100%; border-radius: inherit; }
    /* Inset, so a person or community stands a step smaller than an app; a narrower grid
       shrinks the picture with the tile. */
    .stm-box.is-bare { padding: 7.5px; }
    .stm-box.is-bare > * { max-width: 100%; max-height: 100%; font-size: 27px; }
    .stm-box.is-bare > :not(.round) { border-radius: inherit; }
    .stm-tile:hover .stm-box, .stm-tile:focus-visible .stm-box { border-color: var(--icon-color-primary); }
    .stm-tile:hover .stm-label { color: #fff; }
    .stm-tile.missing .stm-box { opacity: 0.5; }
    .stm-label {
        max-width: 100%;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
        font-size: 13px;
        color: rgba(255, 255, 255, 0.6);
    }
    .stm-folder { display: grid; grid-template-columns: repeat(2, 22px); grid-auto-rows: 22px; gap: 6px; align-content: center; justify-content: center; }
    .stm-mini { display: flex; }
    .stm-add .icon { position: static; width: 34px; height: 34px; margin: 0; background-color: #bbb; }
    .stm-tile.drop-into .stm-box { border-color: var(--icon-color-primary); background-color: #263; transform: scale(1.08); }
    /* The tile beside a drop line leans away from it, opening the gap the pin will fill. */
    .stm-tile.drop-before .stm-box { transform: translateX(6px); }
    .stm-tile.drop-after .stm-box { transform: translateX(-6px); }
    .stm-tile.drop-before::before, .stm-tile.drop-after::after {
        content: '';
        position: absolute;
        top: 6px;
        bottom: 26px;
        width: 3px;
        border-radius: 2px;
        background-color: var(--icon-color-primary);
        box-shadow: 0 0 8px var(--icon-color-primary);
        animation: stm-mark-in 0.14s ease-out;
    }
    .stm-tile.drop-before::before { left: -9px; }
    .stm-tile.drop-after::after { right: -9px; }
    @keyframes stm-mark-in { from { opacity: 0; transform: scaleY(0.4); } }
    /* A held touch says "drag me" before anything moves. */
    .stm-tile.is-armed .stm-box { transform: scale(1.06); box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5); }
    .stm-tile.is-dragging .stm-box { opacity: 0.3; transform: scale(0.9); }
    .stm-tile.is-dragging .stm-label { opacity: 0.3; }
    :global(.stm-ghost) {
        z-index: 9500;
        margin: 0;
        pointer-events: none;
        border-color: var(--icon-color-primary) !important;
        box-shadow: 0 14px 34px rgba(0, 0, 0, 0.55);
        transition: none !important;
        animation: stm-lift 0.14s ease-out forwards;
    }
    @keyframes -global-stm-lift { to { transform: scale(1.08); } }

    .stm-img { border-radius: 10px; object-fit: cover; }
    .stm-img.round, .stm-initial.round { border-radius: 50%; }
    .stm-initial {
        display: flex;
        align-items: center;
        justify-content: center;
        border-radius: 10px;
        background-color: #333;
        font-weight: 700;
        color: #ddd;
    }
    .stm-glyph { display: flex; align-items: center; justify-content: center; }
    .stm-glyph .icon { position: static; width: 70%; height: 70%; margin: 0; background-color: #ddd; }
    /* A pinned place's glyph matches Add Pin's. */
    .stm-box > .stm-glyph .icon { width: 34px; height: 34px; }

    .stm-results { display: flex; flex-direction: column; gap: 2px; }
    .stm-result {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 5px 10px;
        border-radius: 10px;
        cursor: pointer;
    }
    .stm-result.selected { background-color: #2a2a2a; }
    .stm-result-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
    .stm-result-label { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 14px; line-height: 1.2; }
    .stm-result-sub { font-size: 12px; line-height: 1.2; color: rgba(255, 255, 255, 0.45); }
    .stm-act { all: unset; display: flex; padding: 6px; border-radius: 8px; cursor: pointer; opacity: 0.35; }
    .stm-act .icon { position: static; width: 16px; height: 16px; margin: 0; background-color: #fff; }
    .stm-act:hover { background-color: rgba(255, 255, 255, 0.08); }
    .stm-pin.on { opacity: 1; }
    .stm-pin.on .icon { background-color: var(--icon-color-primary); }
    .stm-result:hover .stm-act, .stm-result.selected .stm-act { opacity: 1; }
    .stm-head.hidden { display: none; }
    .stm-none {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 6px;
        padding: 56px 20px 24px;
        text-align: center;
    }
    .stm-none-glyph {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 64px;
        height: 64px;
        margin-bottom: 10px;
        border: 1px solid #2c2c2c;
        border-radius: 50%;
        background: radial-gradient(circle at 50% 35%, #262626, #181818);
        color: #5B6060;
    }
    .stm-none-title {
        display: flex;
        gap: 5px;
        max-width: 100%;
        margin: 0;
        font-size: 15px;
        font-weight: 600;
        color: #e6e6e6;
    }
    .stm-none-query {
        min-width: 0;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
        color: #fff;
    }
    .stm-none-sub { margin: 0; font-size: 13px; color: #7a7a7a; }
    .stm-none-action {
        all: unset;
        display: flex;
        align-items: center;
        gap: 8px;
        margin-top: 16px;
        padding: 9px 16px;
        border: 1px solid #2c2c2c;
        border-radius: 20px;
        background-color: #222;
        font-size: 13px;
        font-weight: 500;
        color: #e6e6e6;
        cursor: pointer;
        transition: background-color 0.12s ease, border-color 0.12s ease;
    }
    .stm-none-action:hover { background-color: #2a2a2a; border-color: #3a3a3a; }
    .stm-none-action .icon { position: static; width: 16px; height: 16px; margin: 0; background-color: var(--icon-color-primary); }

    .stm-foot {
        display: flex;
        align-items: center;
        gap: 6px;
        padding: 16px 26px 18px;
        background-color: #0d0d0d;
    }
    .stm-me { display: flex; align-items: center; gap: 4px; min-width: 0; transform-origin: 0 0; }
    .stm-me-main { all: unset; display: flex; align-items: center; gap: 14px; min-width: 0; cursor: pointer; }
    .stm-me-avatar { display: flex; flex: none; }
    .stm-me-text { display: flex; flex-direction: column; gap: 3px; min-width: 0; max-width: 220px; }
    .stm-me-name { font-size: 18px; font-weight: 600; }
    .stm-me-npub { font-size: 13px; color: rgba(255, 255, 255, 0.5); }
    /* The rail row's caret, grown with it. */
    .stm-caret {
        flex: none;
        width: 0;
        height: 0;
        margin-right: 10px;
        border-left: 7px solid transparent;
        border-right: 7px solid transparent;
        border-top: 9px solid rgba(255, 255, 255, 0.85);
    }
    .stm-me-main:hover .stm-caret { border-top-color: #fff; }
    .stm-foot-gap { flex: 1; }
    .stm-audio { display: flex; align-items: center; border-radius: 10px; }
    .stm-audio:hover { background-color: #1c1c1c; }
    .stm-audio-btn, .stm-audio-more { all: unset; display: flex; padding: 7px; border-radius: 10px; cursor: pointer; }
    .stm-audio-btn .icon { position: static; width: 26px; height: 26px; margin: 0; background-color: #888; }
    .stm-audio-btn:hover .icon { background-color: #bbb; }
    .stm-audio-btn.off .icon { background-color: var(--danger-color); }
    .stm-audio-more { padding: 7px 4px; }
    .stm-audio-more .icon { position: static; width: 14px; height: 14px; margin: 0; background-color: #888; }
</style>
