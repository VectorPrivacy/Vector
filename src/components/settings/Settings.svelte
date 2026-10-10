<script>
    // The Settings screen. On a phone it is one scroll, a section per concern; in
    // widescreen it takes the list column for a SectionNav (the same nav as Community
    // Settings) and shows one category at a time. Both layouts render the same section
    // bodies. Toggle values live in the screen store; the sections whose owners register
    // later (updates, network, voice) render once their handler bag exists.
    import { tick } from 'svelte';
    import { settingsScreen, settingsHandlers } from '../lib/settings.svelte.js';
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

    let blockedOpen = $state(false);
    let updatesEl = $state(null);
    let routingEl = $state(null);
    let networkEl = $state(null);

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
                { id: 'storage', label: 'Storage', icon: 'folder', keys: 'storage usage clear auto-download download limit gallery hide media video quality compression compress' },
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

    async function goCategory(id) {
        if (catId === id) return;
        catId = id;
        await tick();
        anchors.top(category.anchors[0]?.id);
    }

    async function goAnchor(catIdTo, anchorId) {
        if (catId !== catIdTo) {
            catId = catIdTo;
            await tick();
        }
        anchors.jump(anchorId);
    }

    // A requested scroll (an update is waiting, the network needs attention) lands after the
    // screen has been shown.
    $effect(() => {
        const { target, seq } = sc.scroll;
        if (!seq || !['updates', 'routing', 'relays'].includes(target)) return;
        if (shell.ws) {
            if (target === 'updates') goCategory('updates');
            else if (target === 'relays') goAnchor('network', 'relays');
            else goAnchor('privacy', 'routing');
            return;
        }
        const t = setTimeout(async () => {
            await tick();
            const el = { updates: updatesEl, routing: routingEl, relays: networkEl }[target];
            el?.scrollIntoView({ behavior: 'smooth', block: 'start' });
        }, 100);
        return () => clearTimeout(t);
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
    {#if sc.storage.galleryShown}
        <div class="form-group" style="margin-top: 15px;">
            <label class="toggle-container">
                <span><InfoIcon side="right" align="middle" onclick={() => h.help('gallery')} />Hide Media from Gallery</span>
                <input type="checkbox" checked={sc.storage.galleryHidden} onchange={(e) => h.setGalleryHidden(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
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
    {#if sc.storage.videoShown}
        <div class="form-group st-line">
            <InfoIcon side="right" flex onclick={() => h.help('videoQuality')} />
            <span class="st-line-label">Video Quality</span>
            <Select class="vselect-narrow" options={VIDEO_QUALITIES} value={sc.storage.videoQuality}
                    onchange={(v) => h.setVideoQuality(v)} />
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
    <h2 style="margin-top: 40px">Choose Theme</h2>
    {@render themeBody()}

    {#if sc.platform.voice && hs.voice}
        <div class="settings-section">
            <hr class="divider settings-divider">
            <h2>Voice Settings</h2>
            <div><Voice h={hs.voice} /></div>
        </div>
    {/if}

    <div class="settings-section">
        <hr class="divider settings-divider">
        <h2>Privacy</h2>

        {@render privacyBody()}

        <StreamerCard h={h.streamer} />

        {#if routingOffered()}
            <div class="st-routing" bind:this={routingEl}>
                <h3 class="st-routing-head">Routing</h3>
                {@render routingBody()}
            </div>
        {/if}

        <div style="margin-top: 25px;">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <h3 class="btn" style="font-size: 14px; color: #b2b2b2; margin-bottom: 8px; display: flex; align-items: center; justify-content: center; gap: 6px; -webkit-user-select: none; user-select: none;"
                onclick={() => { blockedOpen = !blockedOpen; }}>
                <span class="icon icon-chevron-down" style="width: 14px; height: 14px; position: relative; margin: 0; flex-shrink: 0; background-color: #b2b2b2; transition: transform 0.2s; {blockedOpen ? 'transform: rotate(180deg);' : ''}"></span>
                Blocked Users
            </h3>
            {#if blockedOpen}
                <div style="overflow: hidden; animation: blockedFadeIn 0.2s ease;">
                    <div><BlockedUsers h={h.blocked} /></div>
                </div>
            {/if}
        </div>
    </div>

    <div class="settings-section">
        <hr class="divider settings-divider">
        <h2>Display</h2>
        <div><Display h={h.display} /></div>
    </div>

    <div class="settings-section">
        <hr class="divider settings-divider">
        <h2>Notifications</h2>
        <div><Notifications h={h.notif} /></div>
    </div>

    {#if h.calls}
        <div class="settings-section">
            <hr class="divider settings-divider">
            <h2>Calls</h2>
            <div><Calls h={h.calls} /></div>
        </div>
    {/if}

    {#if sc.battery.shown}
        <div class="settings-section">
            <hr class="divider settings-divider">
            <h2>Battery</h2>
            {@render batteryBody()}
        </div>
    {/if}

    <div class="settings-section" bind:this={networkEl}>
        <hr class="divider settings-divider">
        <h2>Network</h2>
        <div id="network-list" class="network-list">
            {#if hs.network}<NetworkList h={hs.network} />{/if}
        </div>
    </div>

    <div class="settings-section">
        <hr class="divider settings-divider">
        <h2>Storage</h2>
        {@render storageBody()}
    </div>

    {#if sc.platform.updates}
        <div class="settings-section" bind:this={updatesEl}>
            <hr class="divider settings-divider">
            <h2>Updates</h2>
            <div>{#if hs.updates}<Updates h={hs.updates} />{/if}</div>
        </div>
    {/if}

    <div class="settings-section">
        <hr class="divider settings-divider">
        <h2>Security</h2>
        <SecurityCard h={h.security} />
    </div>

    <div class="settings-section">
        <hr class="divider settings-divider">
        <img class="aggro-glitch-img" alt="">
        <h2 class="danger-title" style="margin-bottom: 0; margin-top: 0;">Dangerzone</h2>
        <p class="danger-subtitle">Irreversible Actions</p>
        <div class="danger-buttons">
            {@render advancedBody()}
            {@render logsRow()}
            {@render logoutRow()}
        </div>
    </div>

    <div class="settings-section">
        <hr class="divider settings-divider" style="background-color: #171717;">
        <div class="settings-footer" style="text-align: center; display: flex; gap: 20px; justify-content: center; flex-wrap: wrap;">
            {@render links()}
        </div>
    </div>
{/if}
