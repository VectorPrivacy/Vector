<script>
    // The Updates section body: version, preview notice, what was found, the beta
    // toggle, the one action button and the download bar, all from updates state.
    import { updatesState } from '../lib/settings.svelte.js';
    let { h } = $props();   // h: check, restart, setBeta(on), explainBeta, explainUpdates

    const u = updatesState();
    const status = $derived.by(() => {
        switch (u.phase) {
            case 'checking': return { text: 'Checking for updates...', color: '' };
            case 'ready': return { text: 'Update ready! Restart to apply.', color: '' };
            case 'error': return { text: u.message, color: '#ff5252' };
            case 'store': return { text: u.message, color: '' };
            case 'available': return { text: u.message, color: '' };
            // A stable build's answer is the card above; a preview's still needs saying.
            case 'no-updates': return u.preview
                ? { text: "No newer build yet. You'll be offered the official build as soon as it releases.", color: '#59fcb3' }
                : { text: '', color: '' };
            default: return { text: '', color: '' };
        }
    });
    const button = $derived.by(() => {
        switch (u.phase) {
            case 'checking': return { text: 'Checking...', disabled: true };
            case 'available': return { text: u.downloadLabel, disabled: false };
            case 'downloading': return { text: 'Downloading...', disabled: true };
            default: return { text: 'Check for Updates', disabled: false };
        }
    });
    const showFound = $derived(u.phase === 'available' || u.phase === 'downloading');
    // What the version row says: a newer build found, a check that found none, or a
    // preview build's standing notice. Nothing until there is something to say.
    const standing = $derived(showFound && u.newVersion ? 'outdated' : u.preview ? 'preview' : u.latest ? 'latest' : null);
    function info(e) { e.preventDefault(); e.stopPropagation(); h.explainBeta(); }
    function explain(e) { e.preventDefault(); e.stopPropagation(); h.explainUpdates?.(); }
</script>

<div class="update-info">
    <div class="upd-version-row">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="upd-version-label">Current Version<span class="icon icon-info btn" onclick={explain}></span></span>
        <span class="upd-version">
            {#if standing === 'outdated'}
                <svg class="upd-badge" viewBox="0 0 22 22" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M11 21C16.5228 21 21 16.5228 21 11C21 5.47715 16.5228 1 11 1C5.47715 1 1 5.47715 1 11C1 16.5228 5.47715 21 11 21Z" fill="#B2B2B2"/>
                    <path d="M11 7V11M11 15H11.01M21 11C21 16.5228 16.5228 21 11 21C5.47715 21 1 16.5228 1 11C1 5.47715 5.47715 1 11 1C16.5228 1 21 5.47715 21 11Z" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            {:else if standing === 'latest'}
                <svg class="upd-badge" viewBox="0 0 22 22" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M8.51448 1.91542C7.90072 2.43846 7.13761 2.75455 6.33378 2.8187C4.45794 2.96839 2.96839 4.45794 2.8187 6.33378C2.75455 7.13761 2.43846 7.90072 1.91542 8.51448C0.69486 9.94671 0.69486 12.0533 1.91542 13.4855C2.43846 14.0993 2.75455 14.8624 2.8187 15.6662C2.96839 17.5421 4.45794 19.0316 6.33378 19.1813C7.13761 19.2455 7.90072 19.5615 8.51448 20.0846C9.94671 21.3051 12.0533 21.3051 13.4855 20.0846C14.0993 19.5615 14.8624 19.2455 15.6662 19.1813C17.5421 19.0316 19.0316 17.5421 19.1813 15.6662C19.2455 14.8624 19.5615 14.0993 20.0846 13.4855C21.3051 12.0533 21.3051 9.94671 20.0846 8.51448C19.5615 7.90072 19.2455 7.13761 19.1813 6.33378C19.0316 4.45794 17.5421 2.96839 15.6662 2.8187C14.8624 2.75455 14.0993 2.43846 13.4855 1.91542C12.0533 0.69486 9.94671 0.69486 8.51448 1.91542Z" fill="#59FCB3"/>
                    <path d="M8.00001 11L10 13L14.5 8.50001M6.33378 2.8187C7.13761 2.75455 7.90072 2.43846 8.51448 1.91542C9.94671 0.69486 12.0533 0.69486 13.4855 1.91542C14.0993 2.43846 14.8624 2.75455 15.6662 2.8187C17.5421 2.96839 19.0316 4.45794 19.1813 6.33378C19.2455 7.13761 19.5615 7.90072 20.0846 8.51448C21.3051 9.94671 21.3051 12.0533 20.0846 13.4855C19.5615 14.0993 19.2455 14.8624 19.1813 15.6662C19.0316 17.5421 17.5421 19.0316 15.6662 19.1813C14.8624 19.2455 14.0993 19.5615 13.4855 20.0846C12.0533 21.3051 9.94671 21.3051 8.51448 20.0846C7.90072 19.5615 7.13761 19.2455 6.33378 19.1813C4.45794 19.0316 2.96839 17.5421 2.8187 15.6662C2.75455 14.8624 2.43846 14.0993 1.91542 13.4855C0.69486 12.0533 0.69486 9.94671 1.91542 8.51448C2.43846 7.90072 2.75455 7.13761 2.8187 6.33378C2.96839 4.45794 4.45794 2.96839 6.33378 2.8187Z" stroke="#030303" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            {:else if standing === 'preview'}
                <svg class="upd-badge preview" viewBox="0 0 17 16" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M3.7002 3.62789L7.0752 6.92264M3.7002 3.62789H1.4502L0.700195 1.43139L1.4502 0.699219L3.7002 1.43139V3.62789ZM13.6444 1.24176L11.6737 3.16562C11.3767 3.45557 11.2282 3.60054 11.1726 3.76772C11.1236 3.91478 11.1236 4.07317 11.1726 4.22023C11.2282 4.3874 11.3767 4.53237 11.6737 4.82233L11.8516 4.99604C12.1486 5.28599 12.2972 5.43096 12.4684 5.48528C12.619 5.53306 12.7813 5.53306 12.9319 5.48528C13.1032 5.43096 13.2517 5.28599 13.5487 4.99604L15.3922 3.19644C15.5907 3.66807 15.7002 4.18462 15.7002 4.72614C15.7002 6.95017 13.8534 8.75306 11.5752 8.75306C11.3005 8.75306 11.0321 8.72685 10.7725 8.67685C10.4079 8.6067 10.2256 8.57156 10.1151 8.58232C9.99757 8.59374 9.93967 8.61095 9.83557 8.66535C9.73762 8.71653 9.63944 8.81237 9.44295 9.0042L4.0752 14.2443C3.45387 14.8509 2.44652 14.8509 1.8252 14.2443C1.20387 13.6378 1.20387 12.6543 1.8252 12.0478L7.19295 6.80769C7.38944 6.61587 7.48762 6.51999 7.54005 6.4244C7.59577 6.32277 7.6134 6.26623 7.6251 6.15153C7.63612 6.04364 7.60012 5.86567 7.52827 5.50974C7.47705 5.25627 7.4502 4.99427 7.4502 4.72614C7.4502 2.50213 9.297 0.699219 11.5752 0.699219C12.3293 0.699219 13.0362 0.896772 13.6444 1.24176ZM8.20027 10.2173L12.3252 14.2443C12.9465 14.8508 13.9539 14.8508 14.5752 14.2443C15.1965 13.6377 15.1965 12.6543 14.5752 12.0477L11.1817 8.73498C10.9414 8.71279 10.7072 8.67048 10.4808 8.60978C10.189 8.53158 9.86887 8.5884 9.65497 8.79721L8.20027 10.2173Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            {/if}
            <span id="current-version">{u.version}</span>
        </span>
    </div>
    {#if standing === 'outdated'}
        <p class="upd-card">Your client is currently out of date. The latest version is {u.newVersion}.</p>
    {:else if standing === 'latest'}
        <p class="upd-card"><span class="upd-lead latest">Your client is up to date.</span> It is currently on the latest release.</p>
    {:else if standing === 'preview'}
        <p class="upd-card"><span class="upd-lead preview">This is a Preview Build.</span> It is released ahead of the official version. Preview Builds are public prototypes used to test new features and surface bugs, so feedback from users helps improve the final release. Expect bugs, unfinished features, and occasional instability.</p>
    {/if}
    {#if showFound && u.changelog}
        <div id="update-changelog" style="margin-top: 20px; margin-bottom: 20px;">
            <h3 style="font-size: 16px; margin-bottom: 10px;">What's New:</h3>
            <div id="changelog-content" style="white-space: pre-line;">{u.changelog}</div>
        </div>
    {/if}

    {#if u.betaRow}
        <div class="form-group" style="margin-bottom: 15px;">
            <label class="toggle-container">
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span><span class="icon icon-info btn notif-info" onclick={info}></span>Beta Updates</span>
                <input type="checkbox" checked={u.beta} onchange={(e) => h.setBeta(e.target.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    {/if}

    {#if u.phase !== 'ready' && u.phase !== 'store'}
        <button id="check-updates-btn" class="btn accept-btn btn-bounce" style="display: block;" disabled={button.disabled} onclick={h.check}>{button.text}</button>
    {/if}
    {#if status.text}
        <p id="update-status-text" style="margin-top: 10px;" style:color={status.color || null}>{status.text}</p>
    {/if}

    {#if u.phase === 'downloading'}
        <div id="update-progress-container" style="margin-top: 20px;">
            <div class="progress-bar-track">
                <div id="update-progress-bar" class="progress-bar-fill" style:width="{u.progress}%"></div>
                <div class="progress-text">{u.progress}%</div>
            </div>
        </div>
    {/if}

    {#if u.phase === 'ready'}
        <button id="restart-update-btn" class="btn" style="display: block; margin-top: 10px; background: linear-gradient(135deg, #59fcb3 0%, #2b976c 100%);" onclick={h.restart}>Restart Now</button>
    {/if}
</div>
