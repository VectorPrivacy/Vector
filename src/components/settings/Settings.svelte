<script>
    // The Settings screen: theme, then one section per concern. Toggle values live in
    // the screen store; the sections whose owners register later (updates, network,
    // voice) render once their handler bag exists.
    import { tick } from 'svelte';
    import { settingsScreen, settingsHandlers, torState } from '../lib/settings.svelte.js';
    import InfoIcon from './InfoIcon.svelte';
    import TorCard from './TorCard.svelte';
    import TorAdvanced from './TorAdvanced.svelte';
    import BlockedUsers from './BlockedUsers.svelte';
    import Display from './Display.svelte';
    import Notifications from './Notifications.svelte';
    import NetworkList from './network/NetworkList.svelte';
    import StorageDonut from './StorageDonut.svelte';
    import Updates from './Updates.svelte';
    import SecurityCard from './SecurityCard.svelte';
    import Voice from './Voice.svelte';

    let { h } = $props();
    // h: setTheme(theme), setPrivacy(key, on), help(key), openLink(key), tor: {...}, blocked: {...},
    //    display: {...}, notif: {...}, storageDonut: {...}, setGalleryHidden(on), setAutoDownload(on),
    //    setAutoDownloadLimit(bytes), clearStorage(), setBackgroundService(on), batteryWarningTap(),
    //    security: {...}, copyLogs(), logout()

    const sc = settingsScreen();
    const hs = settingsHandlers();
    const tor = torState();

    const LIMITS = [
        [1048576, '1 MB'], [5242880, '5 MB'], [10485760, '10 MB'],
        [26214400, '25 MB'], [52428800, '50 MB'], [104857600, '100 MB'],
    ];

    let blockedOpen = $state(false);
    let updatesEl = $state(null);

    // A requested scroll (an update is waiting) lands after the screen has been shown.
    $effect(() => {
        const { target, v } = sc.scroll;
        if (!v || target !== 'updates') return;
        const t = setTimeout(async () => {
            await tick();
            updatesEl?.scrollIntoView({ behavior: 'smooth', block: 'start' });
        }, 100);
        return () => clearTimeout(t);
    });
</script>

<h2 style="margin-top: 40px">Choose Theme</h2>
<select id="theme-select" bind:value={sc.theme} onchange={() => h.setTheme(sc.theme)}>
    <option value="vector">Vector</option>
    <option value="satoshi">サトシ</option>
    <option value="chatstr">Chatstr</option>
    <option value="gifverse">Cosmic</option>
    <option value="pivx">Keep it Purple</option>
    <option value="cyberpunk">Neon</option>
    <option value="monero">XMR</option>
</select>

{#if sc.platform.voice && hs.voice}
    <div id="settings-voice" class="settings-section">
        <hr class="divider settings-divider">
        <h2>Voice Settings</h2>
        <div id="settings-voice-body"><Voice h={hs.voice} /></div>
    </div>
{/if}

<div id="settings-privacy" class="settings-section">
    <hr class="divider settings-divider">
    <h2>Privacy</h2>

    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('webPreviews')} />Display Web Previews</span>
            <input type="checkbox" id="privacy-web-previews-toggle" bind:checked={sc.privacy.webPreviews} onchange={() => h.setPrivacy('webPreviews', sc.privacy.webPreviews)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('stripTracking')} />Prevent URL Tracking</span>
            <input type="checkbox" id="privacy-strip-tracking-toggle" bind:checked={sc.privacy.stripTracking} onchange={() => h.setPrivacy('stripTracking', sc.privacy.stripTracking)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group">
        <label class="toggle-container">
            <span><InfoIcon onclick={() => h.help('sendTyping')} />Send Typing Indicators</span>
            <input type="checkbox" id="privacy-send-typing-toggle" bind:checked={sc.privacy.sendTyping} onchange={() => h.setPrivacy('sendTyping', sc.privacy.sendTyping)}>
            <span class="neon-toggle"></span>
        </label>
    </div>

    {#if sc.platform.tor}
        <!-- Adjacent siblings: the card's CSS fuses it with an expanded disclosure. -->
        <TorCard h={h.tor} />
        <TorAdvanced h={h.tor} />
    {/if}

    <div id="settings-blocked-users" style="margin-top: 25px;">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
        <h3 id="settings-blocked-toggle" class="btn" style="font-size: 14px; color: #b2b2b2; margin-bottom: 8px; display: flex; align-items: center; justify-content: center; gap: 6px; -webkit-user-select: none; user-select: none;"
            onclick={() => { blockedOpen = !blockedOpen; }}>
            <span class="icon icon-chevron-down" style="width: 14px; height: 14px; position: relative; margin: 0; flex-shrink: 0; background-color: #b2b2b2; transition: transform 0.2s; {blockedOpen ? 'transform: rotate(180deg);' : ''}"></span>
            Blocked Users
        </h3>
        {#if blockedOpen}
            <div id="settings-blocked-content" style="overflow: hidden; animation: blockedFadeIn 0.2s ease;">
                <div id="settings-blocked-list"><BlockedUsers h={h.blocked} /></div>
            </div>
        {/if}
    </div>
</div>

<div id="settings-display" class="settings-section">
    <hr class="divider settings-divider">
    <h2>Display</h2>
    <div id="settings-display-body"><Display h={h.display} /></div>
</div>

<div id="settings-notifications" class="settings-section">
    <hr class="divider settings-divider">
    <h2>Notifications</h2>
    <div id="settings-notifications-body"><Notifications h={h.notif} /></div>
</div>

{#if sc.battery.shown}
    <div id="settings-battery" class="settings-section">
        <hr class="divider settings-divider">
        <h2>Battery</h2>
        <div class="form-group">
            <label class="toggle-container">
                <span><InfoIcon onclick={() => h.help('battery')} />Run in Background</span>
                <input type="checkbox" id="battery-bg-service-toggle" checked={sc.battery.enabled} onchange={(e) => h.setBackgroundService(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
        {#if sc.battery.warning}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="battery-warning" class="form-group" style="margin-bottom: 0; padding-bottom: 0; cursor: pointer;" onclick={() => h.batteryWarningTap()}>
                <p style="color: #FCE459; font-size: 13px; display: flex; align-items: center; justify-content: center; gap: 6px;"><span class="icon icon-battery-full" style="position: relative; width: 16px; height: 16px; min-width: 16px; margin: 0; background-color: #FCE459;"></span>Battery Optimization is active</p>
            </div>
        {/if}
    </div>
{/if}

<div id="settings-network" class="settings-section">
    <hr class="divider settings-divider">
    <h2>Network</h2>
    <div id="network-list" class="network-list">
        {#if hs.network}<NetworkList h={hs.network} />{/if}
    </div>
</div>

<div id="settings-storage" class="settings-section">
    <hr class="divider settings-divider">
    <h2>Storage</h2>
    <div id="storage-breakdown"><StorageDonut h={h.storageDonut} /></div>
    {#if sc.storage.galleryShown}
        <div class="form-group" id="storage-gallery-group" style="margin-top: 15px;">
            <label class="toggle-container">
                <span><InfoIcon side="right" align="middle" onclick={() => h.help('gallery')} />Hide Media from Gallery</span>
                <input type="checkbox" id="storage-gallery-toggle" checked={sc.storage.galleryHidden} onchange={(e) => h.setGalleryHidden(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}
    <div class="form-group" style="margin-top: 15px;">
        <label class="toggle-container">
            <span><InfoIcon side="right" align="middle" onclick={() => h.help('autoDownload')} />Auto-Download Media</span>
            <input type="checkbox" id="auto-download-toggle" bind:checked={sc.storage.autoDownload} onchange={() => h.setAutoDownload(sc.storage.autoDownload)}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    <div class="form-group" id="auto-download-limit-group" class:disabled={!sc.storage.autoDownload} style="display: flex; align-items: center; margin-top: 15px;">
        <InfoIcon side="right" flex onclick={() => h.help('autoDownloadLimit')} />
        <span style="flex: 1; text-align: left; color: rgba(255, 255, 255, 0.8);">Auto-Download Limit</span>
        <div class="select-container" style="margin: 0;">
            <select id="auto-download-limit" style="margin-bottom: 0 !important;" disabled={!sc.storage.autoDownload}
                    value={String(sc.storage.limit)} onchange={(e) => h.setAutoDownloadLimit(parseInt(e.currentTarget.value, 10))}>
                {#each LIMITS as [bytes, label] (bytes)}<option value={String(bytes)}>{label}</option>{/each}
            </select>
        </div>
    </div>
    <div class="form-group" style="display: flex; align-items: center; margin-top: 15px;">
        <InfoIcon side="right" flex onclick={() => h.help('clearStorage')} />
        <span style="flex: 1; text-align: left; color: rgba(255, 255, 255, 0.8);">Clear Storage</span>
        <button id="clear-storage-btn" class="btn cancel-btn" style="margin: 0;" disabled={sc.storage.clearing} onclick={() => h.clearStorage()}>{sc.storage.clearing ? 'Clearing...' : 'Clear'}</button>
    </div>
</div>

{#if sc.platform.updates}
    <div id="settings-updates" class="settings-section" bind:this={updatesEl}>
        <hr class="divider settings-divider">
        <h2>Updates</h2>
        <div id="settings-updates-body">{#if hs.updates}<Updates h={hs.updates} />{/if}</div>
    </div>
{/if}

<div id="settings-security" class="settings-section">
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
        <div class="danger-option">
            <div class="left-group">
                <InfoIcon onclick={() => h.help('crashLog')} />
                <span>Copy Logs</span>
            </div>
            <button id="copy-crash-log-btn" class="cancel-btn" onclick={() => h.copyLogs()}>Copy</button>
        </div>
        <div class="danger-option">
            <div class="left-group">
                <InfoIcon onclick={() => h.help('logout')} />
                <span>Logout</span>
            </div>
            <button id="logout-btn" class="danger-btn" onclick={() => h.logout()}>
                <img class="warning-icon" alt="">
                Logout
            </button>
        </div>
    </div>
</div>

<div class="settings-section">
    <hr class="divider settings-divider" style="background-color: #171717;">
    <div class="settings-footer" style="text-align: center; display: flex; gap: 20px; justify-content: center; flex-wrap: wrap;">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="footer-donate" onclick={() => h.openLink('donate')}>Donate</span>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="footer-gitbook" onclick={() => h.openLink('gitbook')}>GitBook</span>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="footer-privacy" onclick={() => h.openLink('privacy')}>Privacy Policy</span>
    </div>
</div>
