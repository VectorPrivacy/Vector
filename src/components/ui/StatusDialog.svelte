<script>
    // The Status dialog: a live avatar-and-pill preview of the status other users will
    // see, the emoji-only composer (created by the opener in the host handed over here,
    // and kept for the app's life), and Clear / Save. The card glides to the upper third
    // while the shared emoji panel is open.
    import { statusDialog } from '../lib/statusdialog.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from './Avatar.svelte';
    let { h } = $props();   // h: composerHost(el), renderPreview(node, text), emoji(e), save(), clear(), close(), backdrop(), key(e)
    const st = statusDialog.state();
    function host(el) { h.composerHost(el); }
    // Twemoji and pack shortcodes are imperative passes over the text node.
    function preview(node, text) {
        const paint = (t) => h.renderPreview(node, t);
        paint(text);
        return { update: paint };
    }
</script>

<svelte:window onkeydown={(e) => { if (st.active && !st.closing) h.key(e); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="status-dialog" class="status-dialog-overlay" class:active={st.active} class:closing={st.closing} class:panel-open={st.panelOpen}
     onclick={(e) => { if (e.target === e.currentTarget) h.backdrop(); }}>
    <div class="status-dialog-card" use:popIn={st.tick}>
        <div class="status-dialog-head">
            <span class="status-dialog-label">Status</span>
            <button class="status-dialog-x" aria-label="Close" onclick={() => h.close()}>&#x2715;</button>
        </div>
        <div class="status-live-row">
            <div class="status-preview-avatar"><Avatar src={st.avatarSrc} size={34} /></div>
            <div class="status-preview" class:status-preview-empty={st.empty}>
                <span class="status-preview-dot"></span>
                <span id="status-preview-text" use:preview={st.text}></span>
            </div>
        </div>
        <div class="status-input-row">
            <div class="status-input-host" use:host></div>
            <span class="status-char-count" class:status-char-low={st.low}>{st.count}</span>
            <button id="status-emoji-btn" aria-label="Add emoji" onclick={(e) => h.emoji(e)}><span class="icon icon-smile-face"></span></button>
        </div>
        <div class="status-dialog-actions">
            <button class="status-btn-clear" class:hidden={st.clearHidden} onclick={() => h.clear()}>Clear Status</button>
            <button class="status-btn-save" onclick={() => h.save()}>Save</button>
        </div>
    </div>
</div>
