<script>
    // The 32px desktop title strip: the lockup (the door home to DMs), the update
    // banner, and on platforms without native buttons the window controls. Bare
    // surfaces carry data-tauri-drag-region, so the strip itself moves the window.
    import { chromeState } from '../lib/chrome.svelte.js';
    import { shellState, shellScreens } from '../lib/shell.svelte.js';
    import { updatesState } from '../lib/settings.svelte.js';
    const chrome = chromeState();
    const st = shellState();
    const screens = shellScreens();
    const updates = updatesState();
    const h = $derived(screens.chrome?.h);   // h: ChromeHelpers (js/chrome.js)
</script>

{#if chrome.on}
<header id="window-chrome" class:is-mac={chrome.mac} data-tauri-drag-region>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chrome-lockup" class="btn" title="Direct Messages" onclick={() => h?.openDmHome()}>
    <svg id="chrome-mark" viewBox="26.2 7 29.3 50.5" xmlns="http://www.w3.org/2000/svg" aria-label="Vector">
        <defs>
        <linearGradient id="ws-logo-grad" x1="40.88" y1="53.75" x2="40.88" y2="15.48" gradientUnits="userSpaceOnUse">
            <stop offset="0" stop-color="#2B976C"/>
            <stop offset="0.5" stop-color="#33DB98"/>
            <stop offset="1" stop-color="#59FCB3"/>
        </linearGradient>
        <linearGradient id="ws-logo-grad-v" x1="40.88" y1="166.17" x2="40.88" y2="27.17" gradientUnits="userSpaceOnUse">
            <stop offset="0" stop-color="#2B976C"/>
            <stop offset="0.5" stop-color="#33DB98"/>
            <stop offset="1" stop-color="#59FCB3"/>
        </linearGradient>
        </defs>
        <path fill="url(#ws-logo-grad)" opacity="0.25" d="M55.4843,36.6172L44.097,46.9653c-1.8244,1.6579-4.6101,1.6579-6.4345,0L26.2752,36.6172c-0.0033-0.0031-0.0062-0.0066-0.0095-0.0097v7.3533c0,1.2687,0.5363,2.4881,1.4713,3.3454l10.0751,9.2388c0.8394,0.7697,1.9288,1.1936,3.0676,1.1936s2.2283-0.4239,3.0677-1.1936l10.075-9.2387c0.9351-0.8574,1.4713-2.0768,1.4713-3.3455v-7.3533C55.4904,36.6106,55.4875,36.6141,55.4843,36.6172L55.4843,36.6172L55.4843,36.6172z"/>
        <path fill="url(#ws-logo-grad)" opacity="0.57" d="M55.4843,24.3607L44.097,34.7088c-1.8244,1.6579-4.6101,1.6579-6.4345,0L26.2752,24.3607c-0.0033-0.0031-0.0062-0.0066-0.0095-0.0097v7.3533c0,1.2687,0.5363,2.4881,1.4713,3.3454l10.0751,9.2388c0.8394,0.7697,1.9288,1.1936,3.0676,1.1936s2.2283-0.4239,3.0677-1.1936l10.075-9.2387c0.9351-0.8574,1.4713-2.0768,1.4713-3.3455V24.351C55.4904,24.3541,55.4875,24.3576,55.4843,24.3607L55.4843,24.3607L55.4843,24.3607z"/>
        <path fill="url(#ws-logo-grad-v)" d="M51.8584,7.0814c-1.9041,0.0491-3.3974,1.6622-3.3974,3.5669v5.9678c0,1.0008-0.4199,1.9557-1.1575,2.6321l-3.2444,2.975c-0.9003,0.8256-2.0381,1.2402-3.1793,1.253c-1.1412-0.0128-2.279-0.4274-3.1793-1.253l-3.2444-2.975c-0.7377-0.6764-1.1575-1.6313-1.1575-2.6321v-5.9678c0-1.9047-1.4933-3.5178-3.3974-3.5669c-1.9693-0.0508-3.5816,1.5305-3.5816,3.4883v8.6901c0,1.2687,0.5363,2.4881,1.4713,3.3454l10.0751,9.2387c0.8259,0.7574,1.8948,1.174,3.0139,1.1873c1.119-0.0133,2.1879-0.4299,3.0139-1.1873l10.0751-9.2387c0.935-0.8573,1.4713-2.0767,1.4713-3.3454v-8.6901c0-1.9578-1.6124-3.5391-3.5816-3.4883L51.8584,7.0814z"/>
        <circle fill="url(#ws-logo-grad)" cx="40.8797" cy="14.0653" r="4.6062"/>
    </svg>
    <svg id="chrome-typeface" viewBox="0 0 81.1907 10.7174" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
        <path fill="#fff" d="M9.1578,1.2305c-.5799,0-1.05.47-1.05,1.05v5.7803h-3.3545l-2.5721-2.5721v-3.2082c0-.58-.4701-1.05-1.05-1.05S.0812,1.7005.0812,2.2805v3.6432c0,.2785.1106.5455.3075.7424l3.1872,3.1871c.197.1971.464.3076.7425.3076h4.8394c.5799,0,1.05-.47,1.05-1.05V2.2805c0-.58-.4701-1.05-1.05-1.05Z"/>
        <path fill="#fff" d="M24.2465,8.0608h-2.1642l2.3-2.3001c.1969-.1969.3075-.4639.3075-.7424v-2.4734c0-.58-.4701-1.05-1.05-1.05h-7.8866c-.5799,0-1.05.47-1.05,1.05v6.5659c0,.58.4701,1.05,1.05,1.05h8.4933c.5799,0,1.05-.47,1.05-1.05s-.4701-1.05-1.05-1.05ZM16.8032,8.0608V3.5948h5.7867v.9885l-3.4775,3.4775h-2.3092Z"/>
        <path fill="#fff" d="M38.402,8.0608h-5.025l-1.9517-1.9517v-2.3727h5.74v1.221c0,.58.4701,1.05,1.05,1.05s1.05-.47,1.05-1.05v-2.271c0-.58-.4701-1.05-1.05-1.05h-7.84c-.5799,0-1.05.47-1.05,1.05v3.8577c0,.2785.1106.5455.3075.7424l2.5667,2.5667c.197.1971.464.3076.7425.3076h5.46c.5799,0,1.05-.47,1.05-1.05s-.4701-1.05-1.05-1.05Z"/>
        <path fill="#fff" d="M52.922,5.6419h-.0069c-.5768,0-1.0461.4657-1.0499,1.0432l-.0088,1.3757h-3.8184l-2.2393-2.2394v-2.0616h7.0699c.5799,0,1.05-.47,1.05-1.05s-.4701-1.05-1.05-1.05h-7.0699v-.599c0-.58-.4701-1.05-1.05-1.05s-1.05.47-1.05,1.05v5.1956c0,.2785.1106.5455.3075.7424l2.8544,2.8545c.1969.1971.464.3076.7425.3076h5.2966c.5773,0,1.0463-.4659,1.05-1.0432l.0156-2.4189c.0037-.58-.4634-1.0531-1.0432-1.0568Z"/>
        <path fill="#fff" d="M67.3896,1.7375h-7.8867c-.5799,0-1.05.47-1.05,1.05v6.3233c0,.2789.1109.5463.3083.7432.1969.1965.4636.3068.7417.3068h.0024l5.1917-.0117c.2777-.0006.5438-.1112.7401-.3076l2.695-2.6949c.1969-.1969.3075-.4639.3075-.7424v-3.6167c0-.58-.4701-1.05-1.05-1.05ZM66.3396,5.9692l-2.0809,2.0809-3.7058.0082V3.8374h5.7868v2.1318Z"/>
        <path fill="#fff" d="M80.0595,1.4924h-3.4416c-.2785,0-.5456.1105-.7425.3076l-.9025.9026v-.1625c0-.58-.4701-1.05-1.05-1.05s-1.05.47-1.05,1.05v7.1166c0,.58.4701,1.05,1.05,1.05s1.05-.47,1.05-1.05v-3.9843l2.08-2.08h1.9566v.8984c0,.58.4701,1.05,1.05,1.05s1.05-.47,1.05-1.05v-1.9484c0-.58-.4701-1.05-1.05-1.05Z"/>
    </svg>
        {#if st.mailBadge}<span class="ws-rail-item-badge">{st.mailBadge}</span>{/if}
    </div>
    {#if updates.phase === 'available' && updates.newVersion}
        <button class="chrome-update" onclick={() => h?.openUpdates()}>New update · Vector v{updates.newVersion}. Download now.</button>
    {/if}
    {#if !chrome.mac}
    <div id="chrome-controls">
        <button class="chrome-ctl" title="Minimize" aria-label="Minimize" onclick={() => h?.minimize()}>
            <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5h10" /></svg>
        </button>
        <button class="chrome-ctl" title={chrome.maximized ? 'Restore' : 'Maximize'} aria-label={chrome.maximized ? 'Restore' : 'Maximize'} onclick={() => h?.toggleMaximize()}>
            {#if chrome.maximized}
                <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z" /></svg>
            {:else}
                <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M.5 .5h9v9h-9z" /></svg>
            {/if}
        </button>
        <button class="chrome-ctl chrome-close" title="Close" aria-label="Close" onclick={() => h?.close()}>
            <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M.5 .5l9 9M9.5 .5l-9 9" /></svg>
        </button>
    </div>
    {/if}
</header>
{/if}
