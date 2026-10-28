<script>
    // The Settings screen. In widescreen it takes the list column for a SectionNav (the
    // same nav as Community Settings) and shows one category at a time; on a phone the same
    // categories are an accordion, one open at a time, under the same search. Both layouts
    // render the same section bodies. Toggle values live in the screen store; the sections
    // whose owners register later (updates, network, voice) render once their handler bag exists.
    import { tick, untrack } from 'svelte';
    import { settingsScreen, settingsHandlers, updatesState } from '../lib/settings.svelte.js';
    import { transportState, viewedKind, routingOffered } from '../lib/transport.svelte.js';
    import { advancedState } from '../lib/advanced.svelte.js';
    import { playersState } from '../lib/players.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    import { anchorScroll } from '../lib/anchorscroll.svelte.js';
    import InfoIcon from './InfoIcon.svelte';
    import TransportCard from './TransportCard.svelte';
    import StreamerCard from './StreamerCard.svelte';
    import TorOptions from './TorOptions.svelte';
    import I2pOptions from './I2pOptions.svelte';
    import BlockedUsers from './BlockedUsers.svelte';
    import Display from './Display.svelte';
    import Notifications from './Notifications.svelte';
    import NetworkList from './network/NetworkList.svelte';
    import StorageDonut from './StorageDonut.svelte';
    import Updates from './Updates.svelte';
    import SecurityCard from './SecurityCard.svelte';
    import Voice from './Voice.svelte';
    import Calls from './Calls.svelte';
    import SectionNav from './SectionNav.svelte';
    import Select from '../ui/Select.svelte';

    let { h } = $props();
    // h: setTheme(theme), setPrivacy(key, on), help(key), openLink(key), streamer: {...}, transport: {...}, tor: {...}, blocked: {...},
    //    display: {...}, notif: {...}, storageDonut: {...}, setGalleryHidden(on), setAutoDownload(on),
    //    setAutoDownloadLimit(bytes), setVideoQuality(quality), clearStorage(), setBackgroundService(on), batteryWarningTap(),
    //    security: {...}, setAdvancedMode(on), setPlayers(on), copyLogs(), logout()

    const sc = settingsScreen();
    const hs = settingsHandlers();
    const net = transportState();
    // The panel under the network card: the viewed network's own settings, when this build has it.
    const panel = $derived(net.view?.supported.includes(viewedKind()) ? viewedKind() : '');
    const shell = shellState();
    const adv = advancedState();
    const players = playersState();

    const LIMITS = [
        [1048576, '1 MB'], [5242880, '5 MB'], [10485760, '10 MB'],
        [26214400, '25 MB'], [52428800, '50 MB'], [104857600, '100 MB'],
    ].map(([value, label]) => ({ value, label }));
    const VIDEO_QUALITIES = [
        ['small', 'Small'], ['balanced', 'Balanced'], ['high', 'High'], ['original', 'Original'],
    ].map(([value, label]) => ({ value, label }));

    // Each theme's swatch is its palette, top to bottom; Vector wears its own logo instead.
    const THEMES = [
        { value: 'vector', label: 'Vector', logo: './icons/vector-mark.svg' },
        { value: 'pivx', label: 'Keep it Purple', swatch: 'linear-gradient(#560ec4, #1c0244)' },
        { value: 'satoshi', label: 'サトシ', swatch: 'linear-gradient(#e5953c, #ae712c)' },
        { value: 'chatstr', label: 'Chatstr', swatch: 'linear-gradient(#b949cd, #5949db)' },
        { value: 'gifverse', label: 'Cosmic', swatch: 'linear-gradient(#eab1d3, #baaef6 25%, #bdede6 62%, #f0febd)' },
        { value: 'monero', label: 'XMR', swatch: '#ed702d' },
        { value: 'cyberpunk', label: 'Neon', swatch: 'linear-gradient(#8bf2f0, #e46aad)' },
    ];

    // ── widescreen: categories of anchored blocks ──
    // An anchor is one headed block; `keys` are what the nav search matches beyond its label.
    const CATEGORIES = $derived([
        {
            id: 'appearance', label: 'Appearance', icon: 'palette',
            anchors: [
                { id: 'theme', label: 'Theme', icon: 'palette', keys: 'theme colour color vector satoshi chatstr cosmic purple neon xmr' },
                { id: 'display', label: 'Display', icon: 'image', keys: 'display image types background wallpaper rich composer emoticon suggestions time suggestions countdown autocorrect send on enter return key floating player' },
            ],
        },
        {
            id: 'audio', label: 'Audio', icon: 'volume-max',
            anchors: [
                ...(sc.platform.voice && hs.voice ? [{ id: 'voice', label: 'Voice', icon: 'mic-on', keys: 'voice whisper transcribe transcription translate model download' }] : []),
                ...(h.calls ? [{ id: 'calls', label: 'Calls', icon: 'volume-max', keys: 'calls microphone speaker automatic gain echo cancellation noise suppression test mic device' }] : []),
            ],
        },
        {
            id: 'notifications', label: 'Notifications', icon: 'bell',
            anchors: [
                { id: 'notifications', label: 'Notifications', icon: 'bell', keys: 'mute sounds everyone pings content privacy sound prelude techno custom' },
                ...(sc.battery.shown ? [{ id: 'battery', label: 'Background', icon: 'battery-full', keys: 'battery background run optimization' }] : []),
            ],
        },
        {
            id: 'network', label: 'Network', icon: 'globe',
            anchors: [
                { id: 'relays', label: 'Nostr Relays', icon: 'globe', keys: 'network nostr relays add custom' },
                { id: 'servers', label: 'Media Servers', icon: 'folder', keys: 'network media servers blossom upload add custom' },
            ],
        },
        {
            id: 'storage', label: 'Storage', icon: 'folder',
            anchors: [
                { id: 'storage', label: 'Storage', icon: 'folder', keys: 'storage usage clear auto-download download limit gallery show save keep files unencrypted media video quality compression compress' },
            ],
        },
        {
            id: 'privacy', label: 'Privacy', icon: 'eye-off',
            anchors: [
                { id: 'privacy', label: 'Privacy', icon: 'eye-off', keys: 'web previews video players youtube embeds url tracking typing indicators proxy media' },
                { id: 'streamer', label: 'Streamer Mode', icon: 'video', keys: 'streamer stream streaming live obs twitch screen share hide names pictures notifications' },
                ...(routingOffered() ? [{ id: 'routing', label: 'Routing', icon: 'shield-filled', keys: 'routing network tor i2p onion clearnet bridges obfs4 circuit outproxy sam router anonymity' }] : []),
                { id: 'blocked', label: 'Blocked Users', icon: 'x-user', keys: 'blocked users block unblock' },
            ],
        },
        {
            id: 'security', label: 'Security', icon: 'locked',
            anchors: [
                { id: 'security', label: 'Security', icon: 'locked', keys: 'security pin password biometric signer bunker encryption key' },
            ],
        },
        ...(sc.platform.updates ? [{
            id: 'updates', label: 'Updates', icon: 'download',
            anchors: [
                { id: 'updates', label: 'Updates', icon: 'download', keys: 'updates version release changelog download install' },
            ],
        }] : []),
        {
            id: 'danger', label: 'Dangerzone', icon: 'warning', danger: true,
            anchors: [
                { id: 'advanced', label: 'Advanced', icon: 'file-code', keys: 'advanced mode developer bot ids copy id identifier' },
                { id: 'logs', label: 'Logs', icon: 'copy', keys: 'copy logs crash debug' },
                { id: 'logout', label: 'Logout', icon: 'warning', keys: 'logout log out sign out' },
            ],
        },
    ].filter((c) => c.anchors.length));

    let catId = $state('appearance');
    let query = $state('');
    const category = $derived(CATEGORIES.find((c) => c.id === catId) || CATEGORIES[0]);
    const anchors = anchorScroll('theme');

    // The section a nav click or a link brought into view; a category alone names none.
    let section = $state('');

    async function goCategory(id) {
        section = '';
        if (catId === id) return;
        catId = id;
        await tick();
        anchors.top(category.anchors[0]?.id);
    }

    async function goAnchor(catIdTo, anchorId) {
        section = anchorId;
        if (catId !== catIdTo) {
            catId = catIdTo;
            await tick();
        }
        anchors.jump(anchorId);
    }

    // ── phone: the categories as an accordion ──
    // A search opens every category it matches, showing only the blocks it matched, as the
    // widescreen nav does; a category's body mounts the first time it opens and then stays.
    const updates = updatesState();
    let openCat = $state(null);
    let searchShut = $state([]);
    let kept = $state({});
    let phoneEl = $state(null);
    let searchEl = $state(null);
    const rows = {};
    const q = $derived(query.trim().toLowerCase());
    const phoneNav = $derived(CATEGORIES.map((sec) => {
        const hits = sec.anchors.map((a) => !q || `${sec.label} ${a.label} ${a.keys || ''}`.toLowerCase().includes(q));
        const shown = hits.includes(true);
        return {
            ...sec,
            shown,
            open: q ? shown && !searchShut.includes(sec.id) : sec.id === openCat,
            anchors: sec.anchors.map((a, i) => ({ ...a, shown: hits[i] })),
        };
    }));
    // A waiting update leaves the list for a card of its own above it.
    const pending = $derived(['available', 'downloading', 'ready'].includes(updates.phase)
        ? phoneNav.find((sec) => sec.id === 'updates') : null);
    const listed = $derived(phoneNav.filter((sec) => sec !== pending));
    const empty = $derived(!phoneNav.some((sec) => sec.shown));
    $effect(() => {
        for (const sec of phoneNav) if (sec.open && !kept[sec.id]) kept[sec.id] = true;
    });
    // A category clips its body only while it folds: open and still, a dropdown inside it
    // has to spill past its edge. It settles when its fold ends; the timer covers a fold
    // with no transition to end (reduced motion).
    let settled = $state({});
    const openIds = $derived(phoneNav.filter((sec) => sec.open).map((sec) => sec.id).join());
    $effect(() => {
        const ids = openIds.split(',').filter(Boolean);
        untrack(() => { for (const id of Object.keys(settled)) if (!ids.includes(id)) settled[id] = false; });
        const t = setTimeout(() => { for (const id of ids) settled[id] = true; }, 800);
        return () => clearTimeout(t);
    });
    function foldEnd(e, sec) {
        if (e.target === e.currentTarget && e.propertyName === 'grid-template-rows' && sec.open) settled[sec.id] = true;
    }
    // The card's top edge draws no divider, whichever row a search leaves first.
    const firstListed = $derived(listed.find((sec) => sec.shown)?.id);

    function setQuery(v) {
        query = v;
        searchShut = [];
    }

    function toggle(id) {
        if (q) {
            searchShut = searchShut.includes(id) ? searchShut.filter((x) => x !== id) : [...searchShut, id];
            return;
        }
        section = '';
        openCat = openCat === id ? null : id;
        if (openCat) reveal(id);
    }

    // Once the category that was open has folded, so the row is aimed at where it settles.
    function reveal(id, anchorId = null) {
        setTimeout(() => {
            const el = anchorId ? phoneEl?.querySelector(`[data-anchor="${anchorId}"]`) : rows[id];
            el?.scrollIntoView({ behavior: 'smooth', block: 'start' });
        }, 300);
    }

    // A requested scroll (a link, an update waiting, the network needing attention) lands after
    // the screen has been shown. `category[/section]`, or a bare section id; anything this build
    // doesn't have leaves the screen where it is.
    $effect(() => {
        const { target, seq } = sc.scroll;
        if (!seq || !target) return;
        untrack(() => {
            const [first, second] = target.split('/');
            const cat = CATEGORIES.find((c) => c.id === first)
                || (!second && CATEGORIES.find((c) => c.anchors.some((a) => a.id === first)));
            if (!cat) return;
            const anchorId = second || (cat.id === first ? '' : first);
            if (anchorId && !cat.anchors.some((a) => a.id === anchorId)) return;
            if (shell.ws) {
                if (anchorId) goAnchor(cat.id, anchorId);
                else goCategory(cat.id);
                return;
            }
            setQuery('');
            openCat = cat.id;
            section = anchorId;
            reveal(cat.id, anchorId || null);
        });
    });

    // Where the screen is, for a link to here (Vector Web's address bar follows it).
    $effect(() => {
        sc.at = { category: shell.ws ? catId : (openCat || ''), section };
    });
</script>

<!-- ── Section bodies, shared by both layouts ── -->

{#snippet themeLead(t)}
    {#if t.logo}<img class="theme-logo" src={t.logo} alt="">{:else}<span class="theme-swatch" style:background={t.swatch}></span>{/if}
{/snippet}

{#snippet themeBody()}
    <Select class="theme-picker" options={THEMES} value={sc.theme} lead={themeLead}
            onchange={(v) => { sc.theme = v; h.setTheme(v); }} />
{/snippet}

{#snippet privacyBody()}
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('webPreviews')} />Display Web Previews</span>
            <input type="checkbox" bind:checked={sc.privacy.webPreviews} onchange={() => h.setPrivacy('webPreviews', sc.privacy.webPreviews)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    {#if players.supported}
        <div class="form-group">
            <label class="toggle-container">
                <span><InfoIcon onclick={() => h.help('players')} />Video Players</span>
                <input type="checkbox" checked={players.on}
                       onchange={async (e) => { const box = e.currentTarget; await h.setPlayers(box.checked); box.checked = players.on; }}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('stripTracking')} />Prevent URL Tracking</span>
            <input type="checkbox" bind:checked={sc.privacy.stripTracking} onchange={() => h.setPrivacy('stripTracking', sc.privacy.stripTracking)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('sendTyping')} />Send Typing Indicators</span>
            <input type="checkbox" bind:checked={sc.privacy.sendTyping} onchange={() => h.setPrivacy('sendTyping', sc.privacy.sendTyping)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('proxyMedia')} />Proxy Previews &amp; Media</span>
            <input type="checkbox" bind:checked={sc.privacy.proxyMedia} onchange={() => h.setPrivacy('proxyMedia', sc.privacy.proxyMedia)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/snippet}

{#snippet routingBody()}
    <TransportCard h={h.transport} />
    {#if panel === 'tor'}<TorOptions h={h.tor} />
    {:else if panel === 'i2p'}<I2pOptions h={h.transport} />
    {/if}
{/snippet}

{#snippet batteryBody()}
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('battery')} />Run in Background</span>
            <input type="checkbox" checked={sc.battery.enabled} onchange={(e) => h.setBackgroundService(e.currentTarget.checked)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    {#if sc.battery.warning}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="form-group" style="margin-bottom: 0; padding-bottom: 0; cursor: pointer;" onclick={() => h.batteryWarningTap()}>
            <p style="color: #FCE459; font-size: 13px; display: flex; align-items: center; justify-content: center; gap: 6px;"><span class="icon icon-battery-full" style="position: relative; width: 16px; height: 16px; min-width: 16px; margin: 0; background-color: #FCE459;"></span>Battery Optimization is active</p>
        </div>
    {/if}
{/snippet}

{#snippet storageBody()}
    <div><StorageDonut h={h.storageDonut} /></div>
    <div class="form-group" style="margin-top: 15px;">
        <label class="toggle-container">
            <span><InfoIcon side="right" align="middle" onclick={() => h.help('autoDownload')} />Auto-Download Media</span>
            <input type="checkbox" bind:checked={sc.storage.autoDownload} onchange={() => h.setAutoDownload(sc.storage.autoDownload)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group st-line" id="auto-download-limit-group" class:disabled={!sc.storage.autoDownload}>
        <InfoIcon side="right" flex onclick={() => h.help('autoDownloadLimit')} />
        <span class="st-line-label">Auto-Download Limit</span>
        <Select class="vselect-narrow" options={LIMITS} value={sc.storage.limit} disabled={!sc.storage.autoDownload}
                onchange={(v) => h.setAutoDownloadLimit(v)} />
    </div>
    {#if sc.storage.keepShown}
        <div class="form-group" style="margin-top: 15px;">
            <label class="toggle-container">
                <span><InfoIcon side="right" align="middle" onclick={() => h.help('keepMedia')} />Keep Files on This Device</span>
                <input type="checkbox" checked={sc.storage.keepMedia} onchange={(e) => h.setKeepMedia(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
    {#if sc.storage.plainShown && (adv.on || sc.storage.keepUnencrypted) && sc.storage.keepMedia}
        <div class="form-group" style="margin-top: 15px;">
            <label class="toggle-container">
                <span><InfoIcon side="right" align="middle" onclick={() => h.help('keepUnencrypted')} />Keep Unencrypted</span>
                <input type="checkbox" checked={sc.storage.keepUnencrypted} onchange={(e) => h.setKeepUnencrypted(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
    {#if sc.storage.galleryShown}
        <div class="form-group" style="margin-top: 15px;">
            <label class="toggle-container">
                <span><InfoIcon side="right" align="middle" onclick={() => h.help('gallery')} />Show in Gallery</span>
                <input type="checkbox" checked={!sc.storage.galleryHidden} onchange={(e) => h.setGalleryHidden(!e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
    {#if sc.storage.videoShown}
        <div class="form-group st-line">
            <InfoIcon side="right" flex onclick={() => h.help('videoQuality')} />
            <span class="st-line-label">Video Quality</span>
            <Select class="vselect-narrow" options={VIDEO_QUALITIES} value={sc.storage.videoQuality}
                    onchange={(v) => h.setVideoQuality(v)} />
        </div>
    {/if}
    {#if sc.storage.moving}
        <div class="form-group st-line">
            <span class="st-line-label">{sc.storage.movingPlain ? 'Decrypting your kept files' : 'Moving your files into Vector'}</span>
            <span>{sc.storage.moving}</span>
        </div>
    {/if}
    <div class="form-group st-line">
        <InfoIcon side="right" flex onclick={() => h.help('clearStorage')} />
        <span class="st-line-label">Clear Storage</span>
        <button class="btn cancel-btn" style="margin: 0;" disabled={sc.storage.clearing} onclick={() => h.clearStorage()}>{sc.storage.clearing ? 'Clearing...' : 'Clear'}</button>
    </div>
{/snippet}

{#snippet advancedBody()}
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('advancedMode')} />Advanced Mode</span>
            <!-- After the save, the box shows what stuck: a refused change leaves the store as it was. -->
            <input type="checkbox" checked={adv.on}
                   onchange={async (e) => { const box = e.currentTarget; await h.setAdvancedMode(box.checked); box.checked = adv.on; }}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/snippet}

{#snippet logsRow()}
    <div class="danger-option">
        <div class="left-group">
            <InfoIcon onclick={() => h.help('crashLog')} />
            <span>Copy Logs</span>
        </div>
        <button class="cancel-btn" onclick={() => h.copyLogs()}>Copy</button>
    </div>
{/snippet}

{#snippet logoutRow()}
    <div class="danger-option">
        <div class="left-group">
            <InfoIcon onclick={() => h.help('logout')} />
            <span>Logout</span>
        </div>
        <button class="danger-btn" onclick={() => h.logout()}>
            <img class="warning-icon" alt="">
            Logout
        </button>
    </div>
{/snippet}

{#snippet links()}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <span onclick={() => h.openLink('donate')}>Donate</span>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <span onclick={() => h.openLink('gitbook')}>GitBook</span>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <span onclick={() => h.openLink('privacy')}>Privacy Policy</span>
{/snippet}

<!-- A phone category's body: its blocks, headed when there are several, folding open under its row. -->
{#snippet catBody(sec)}
    <div class="sm-body" ontransitionend={(e) => foldEnd(e, sec)}>
        <div class="sm-body-inner">
            {#if kept[sec.id]}
                <div class="st-page sm-page">
                    {#if sec.danger}
                        <div class="sm-danger-head">
                            <img class="aggro-glitch-img" alt="">
                            <p class="danger-subtitle">Irreversible Actions</p>
                        </div>
                    {/if}
                    {#each sec.anchors as a (a.id)}
                        <section class="sm-block" class:off={!a.shown} data-anchor={a.id}>
                            {#if sec.anchors.length > 1}<h3 class="sm-heading">{a.label}</h3>{/if}
                            {@render block(a.id)}
                        </section>
                    {/each}
                </div>
            {/if}
        </div>
    </div>
{/snippet}

<!-- One anchored block's body, by anchor id. -->
{#snippet block(id)}
    {#if id === 'theme'}
        <div class="form-group st-line">
            <span class="st-line-label">Change Theme</span>
            {@render themeBody()}
        </div>
    {:else if id === 'display'}<Display h={h.display} grouped />
    {:else if id === 'voice'}<Voice h={hs.voice} />
    {:else if id === 'calls'}<Calls h={h.calls} />
    {:else if id === 'notifications'}<Notifications h={h.notif} />
    {:else if id === 'battery'}{@render batteryBody()}
    {:else if id === 'relays'}{#if hs.network}<NetworkList h={hs.network} part="relays" />{/if}
    {:else if id === 'servers'}{#if hs.network}<NetworkList h={hs.network} part="servers" />{/if}
    {:else if id === 'storage'}{@render storageBody()}
    {:else if id === 'privacy'}{@render privacyBody()}
    {:else if id === 'streamer'}<StreamerCard h={h.streamer} />
    {:else if id === 'routing'}{@render routingBody()}
    {:else if id === 'blocked'}<BlockedUsers h={h.blocked} />
    {:else if id === 'security'}<SecurityCard h={h.security} />
    {:else if id === 'advanced'}{@render advancedBody()}
    {:else if id === 'updates'}{#if hs.updates}<Updates h={hs.updates} />{/if}
    {:else if id === 'logs'}{@render logsRow()}
    {:else if id === 'logout'}{@render logoutRow()}
    {/if}
{/snippet}

{#if shell.ws}
    <SectionNav sections={CATEGORIES} section={category.id} anchor={anchors.active} {query}
                onquery={(v) => { query = v; }} ongo={goCategory} onanchor={goAnchor}>
        {#snippet head()}
            <span class="cs-nav-glyph"><span class="icon icon-settings"></span></span>
            <span class="cs-nav-title cutoff">Settings</span>
        {/snippet}
        {#snippet footer()}
            <div class="settings-footer">{@render links()}</div>
        {/snippet}
    </SectionNav>

    <main class="cs-main">
        <header class="cs-top">
            <h2 class="cs-top-title">{category.label}</h2>
        </header>
        <div class="cs-scroll" bind:this={anchors.el} onscroll={() => anchors.spy()}>
            {#key category.id}
                <div class="cs-content cs-content-enter st-page" class:st-danger={category.danger}>
                    {#each category.anchors as a (a.id)}
                        <section class="cs-block" data-anchor={a.id}>
                            <h3 class="cs-heading">{a.label}</h3>
                            {@render block(a.id)}
                        </section>
                    {/each}
                </div>
            {/key}
        </div>
    </main>
{:else}
    <div class="sm" bind:this={phoneEl}>
        <h1 class="sm-title">Settings</h1>
        <div class="sm-search" class:has-query={!!query}>
            <span class="icon icon-search"></span>
            <input placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                   value={query} oninput={(e) => setQuery(e.currentTarget.value)} bind:this={searchEl}>
            {#if query}
                <button class="sm-search-clear" type="button" aria-label="Clear search" onclick={() => { setQuery(''); searchEl?.focus(); }}>
                    <svg viewBox="0 0 18 18" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                        <path d="M11.1002 6.29922L6.3002 11.0992M6.3002 6.29922L11.1002 11.0992M16.7002 8.69922C16.7002 13.1175 13.1184 16.6992 8.7002 16.6992C4.28192 16.6992 0.700195 13.1175 0.700195 8.69922C0.700195 4.28094 4.28192 0.699219 8.7002 0.699219C13.1184 0.699219 16.7002 4.28094 16.7002 8.69922Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                </button>
            {/if}
        </div>

        {#if pending}
            <div class="sm-cat sm-update" class:open={pending.open} class:shut={!pending.shown} class:clip={!(pending.open && settled.updates)}>
                <div class="sm-cat-inner">
                    <button class="sm-row" aria-expanded={pending.open} bind:this={rows.updates} onclick={() => toggle('updates')}>
                        <span class="sm-row-icon"><span class="icon icon-refresh"></span></span>
                        <span class="sm-row-label cutoff">Update Pending{#if updates.newVersion}<span class="sm-row-note">– {updates.newVersion}</span>{/if}</span>
                        <span class="sm-badge">1</span>
                        <span class="sm-chevron"><span class="icon icon-chevron-right"></span></span>
                    </button>
                    {@render catBody(pending)}
                </div>
            </div>
        {/if}

        <div class="sm-list">
            {#each listed as sec (sec.id)}
                <div class="sm-cat" class:open={sec.open} class:shut={!sec.shown} class:danger={sec.danger}
                     class:clip={!(sec.open && settled[sec.id])} class:first={sec.id === firstListed}>
                    <div class="sm-cat-inner">
                        <button class="sm-row" aria-expanded={sec.open} bind:this={rows[sec.id]} onclick={() => toggle(sec.id)}>
                            <span class="sm-row-icon"><span class="icon icon-{sec.icon}"></span></span>
                            <span class="sm-row-label cutoff">{sec.label}</span>
                            <span class="sm-chevron"><span class="icon icon-chevron-right"></span></span>
                        </button>
                        {@render catBody(sec)}
                    </div>
                </div>
            {/each}
        </div>
        <p class="sm-empty" class:shown={empty}>No matches</p>

        <div class="settings-footer sm-footer">{@render links()}</div>
    </div>
{/if}
