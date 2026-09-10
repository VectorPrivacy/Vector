<script>
    // The confirm/notice popup, backdrop included. Markup and ids match the CSS. A
    // caller's trusted HTML may carry [data-action] controls; their handlers ride in `actions`.
    import { popupState, popupAnswer } from '../lib/popup.svelte.js';
    const p = popupState();
    function contentClick(e) {
        const el = e.target.closest('[data-action]');
        const fn = el && p.actions?.[el.dataset.action];
        if (fn) fn();
    }
    let input = $state(null);
    // A bare filename resolves under ./icons/; a full URL (asset/blob/data/http, e.g. a
    // decrypted community logo) is used verbatim.
    const iconSrc = $derived(/:\/\/|^data:|^blob:/.test(p.icon) ? p.icon : './icons/' + p.icon);
    $effect(() => { if (p.open && p.placeholder && input) input.focus(); });
</script>

<div id="popup-container" class="popup-container" class:active={p.open}>
<center id="popup" class="popup" style:display={p.open ? '' : 'none'}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="popup-content" onclick={contentClick}>
        <img id="popupIcon" src={iconSrc} style="height: 100px;" style:display={p.icon ? '' : 'none'} class:popup-icon-circular={p.circular} alt="">
        <h2 class={p.titleClass}>{p.title}</h2>
        <p id="popupSubtext">{@html p.html}</p>
        {#if p.placeholder}
            <input autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" placeholder={p.placeholder} bind:value={p.value} bind:this={input}>
        {/if}
    </div>
    <div class="popup-buttons">
        <button id="popupConfirm" onclick={() => popupAnswer()?.confirm()}>{p.confirmText}</button><button class="cancel-btn" style:display={p.notice ? 'none' : ''} onclick={() => popupAnswer()?.cancel()}>Cancel</button>
    </div>
</center>
</div>
