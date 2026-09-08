<script>
    // The confirm/notice popup. Markup and ids match the CSS; the container element
    // outside this component keeps the `active` backdrop class from an effect.
    import { popupState, popupAnswer } from '../lib/popup.svelte.js';
    let { container } = $props();
    const p = popupState();
    let input = $state(null);
    // A bare filename resolves under ./icons/; a full URL (asset/blob/data/http, e.g. a
    // decrypted community logo) is used verbatim.
    const iconSrc = $derived(/:\/\/|^data:|^blob:/.test(p.icon) ? p.icon : './icons/' + p.icon);
    $effect(() => { container.classList.toggle('active', p.open); });
    $effect(() => { if (p.open && p.placeholder && input) input.focus(); });
</script>

<center id="popup" class="popup" style:display={p.open ? '' : 'none'}>
    <div class="popup-content">
        <img id="popupIcon" src={iconSrc} style="height: 100px;" style:display={p.icon ? '' : 'none'} class:popup-icon-circular={p.circular} alt="">
        <h2 id="popupTitle" class={p.titleClass}>{p.title}</h2>
        <p id="popupSubtext">{@html p.html}</p>
        {#if p.placeholder}
            <input id="popupInput" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" placeholder={p.placeholder} bind:value={p.value} bind:this={input}>
        {/if}
    </div>
    <div class="popup-buttons">
        <button id="popupConfirm" onclick={() => popupAnswer()?.confirm()}>{p.confirmText}</button><button id="popupCancel" class="cancel-btn" style:display={p.notice ? 'none' : ''} onclick={() => popupAnswer()?.cancel()}>Cancel</button>
    </div>
</center>
