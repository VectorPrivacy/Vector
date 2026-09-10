<script>
    // The Updates section body: version, preview notice, what was found, the beta
    // toggle, the one action button and the download bar, all from updates state.
    import { updatesState } from '../lib/settings.svelte.js';
    let { h } = $props();   // h: check, restart, setBeta(on), explainBeta

    const u = updatesState();
    const status = $derived.by(() => {
        switch (u.phase) {
            case 'checking': return { text: 'Checking for updates...', color: '' };
            case 'ready': return { text: 'Update ready! Restart to apply.', color: '' };
            case 'error': return { text: u.message, color: '#ff5252' };
            case 'store': return { text: u.message, color: '' };
            case 'available': return { text: u.message, color: '' };
            case 'no-updates': return {
                text: u.preview
                    ? "No newer build yet. You'll be offered the official build as soon as it releases."
                    : 'You are running the latest version',
                color: '#59fcb3',
            };
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
    function info(e) { e.preventDefault(); e.stopPropagation(); h.explainBeta(); }
</script>

<div class="update-info">
    <p><span style="opacity: 0.7;">Current Version:</span> <span id="current-version">{u.version}</span></p>
    {#if u.preview}
        <div id="update-preview-notice" class="update-preview-notice">
            <p>
                <img class="warning-icon" alt=""><strong>This is a Preview build.</strong>
                It ships ahead of the official release, so expect rough edges.
            </p>
            <p>
                Updates come from the preview channel, and you'll be moved onto the official
                build the moment it releases.
            </p>
        </div>
    {/if}
    {#if showFound && u.newVersion}
        <p id="new-version-display">New Version: <span id="new-version">{u.newVersion}</span></p>
    {/if}
    {#if showFound && u.changelog}
        <div id="update-changelog" style="margin-top: 20px; margin-bottom: 20px;">
            <h3 style="font-size: 16px; margin-bottom: 10px;">What's New:</h3>
            <div id="changelog-content" style="white-space: pre-line;">{u.changelog}</div>
        </div>
    {/if}

    {#if u.betaRow}
        <div id="beta-updates-row" class="form-group" style="margin-bottom: 15px;">
            <label class="toggle-container">
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span><span class="icon icon-info btn notif-info" onclick={info}></span>Beta Updates</span>
                <input type="checkbox" id="beta-updates-toggle" checked={u.beta} onchange={(e) => h.setBeta(e.target.checked)}>
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
                <div class="progress-text" id="update-progress-text">{u.progress}%</div>
            </div>
        </div>
    {/if}

    {#if u.phase === 'ready'}
        <button id="restart-update-btn" class="btn" style="display: block; margin-top: 10px; background: linear-gradient(135deg, #59fcb3 0%, #2b976c 100%);" onclick={h.restart}>Restart Now</button>
    {/if}
</div>
