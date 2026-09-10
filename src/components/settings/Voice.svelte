<script>
    // The Voice section body: the two toggles, the model picker with its delete
    // control, the status line and the download controls, from voice state.
    import { voiceState } from '../lib/settings.svelte.js';
    let { h } = $props();   // h: formatBytes, explain(kind), setTranslate, setTranscribe, selectModel, download, deleteModel, cancelDownload

    const v = voiceState();
    const current = $derived(v.models.find(m => m.model.name === v.selected) || null);
    const canTranslate = $derived(current ? !!current.model.supports_translate : true);
    const busy = $derived(!!v.download);
    const options = $derived(v.models.map(m => {
        const canRun = v.memoryMB >= (m.model.ram_required || 0);
        let text = m.model.display_name;
        if (!canRun) text += ' (Insufficient RAM)';
        else if (m.model.name === v.recommended) text += ' [Recommended]';
        return { name: m.model.name, text, disabled: !canRun };
    }));
    function info(kind) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(kind); };
    }
</script>

<div class="form-group">
    <label class="toggle-container" class:disabled={!canTranslate}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span><span class="icon icon-info btn notif-info" onclick={info('translate')}></span>Auto-Translate</span>
        <input type="checkbox" checked={canTranslate && v.autoTranslate} disabled={!canTranslate} onchange={(e) => h.setTranslate(e.target.checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>
{#if !canTranslate}
    <div class="form-group" style="margin-bottom: 0; padding-bottom: 0;">
        <p style="color: #FCE459; font-size: 13px; display: flex; align-items: center; justify-content: center; gap: 6px;">
            <span class="icon icon-warning" style="position: relative; width: 16px; height: 16px; min-width: 16px; margin: 0; background-color: #FCE459;"></span>
            Selected model does not support translation
        </p>
    </div>
{/if}

<div class="form-group">
    <label class="toggle-container">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span><span class="icon icon-info btn notif-info" onclick={info('transcribe')}></span>Auto-Transcribe</span>
        <input type="checkbox" checked={v.autoTranscribe} onchange={(e) => h.setTranscribe(e.target.checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>

<div class="form-group" style="margin-top: 30px;">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <label for="whisper-model" style="display: inline-flex; align-items: center;">Whisper Model
        <span class="icon icon-info btn notif-info" style="vertical-align: baseline;" onclick={info('model')}></span></label>
    <div class="model-select-container">
        <select id="whisper-model" class="form-control" style="margin-top: 10px;" disabled={busy || v.loading} value={v.selected} onchange={(e) => h.selectModel(e.target.value)}>
            {#if v.loading}
                <option value="" disabled selected>Loading models...</option>
            {:else if v.error && !v.models.length}
                <option value="" disabled>Error loading models</option>
            {:else}
                {#each options as o (o.name)}
                    <option value={o.name} disabled={o.disabled}>{o.text}</option>
                {/each}
            {/if}
        </select>
        {#if current?.downloaded && !busy}
            <button class="btn-delete-model downloaded" title="Delete {current.model.display_name}" onclick={h.deleteModel}>
                <span class="icon-trash"></span>
            </button>
        {/if}
    </div>
    <div id="model-status" class="model-status">
        {#if busy}
            <div class="alert alert-info"><span class="spinner"></span><span> Downloading... {v.download.progress}</span></div>
        {:else if v.error}
            <div class="alert alert-warning">{v.error}</div>
        {:else if current?.downloaded}
            <div class="alert alert-success">Vector AI is ready</div>
        {:else if current}
            <div class="alert alert-warning">AI model is not downloaded</div>
        {/if}
    </div>
</div>
<div class="form-group">
    {#if current && !current.downloaded && !busy}
        <button id="download-model" class="btn btn-primary" onclick={h.download}>Download Model ({h.formatBytes(current.model.size * 1024 * 1024)})</button>
    {/if}
    {#if busy}
        <button id="cancel-download" class="btn btn-secondary" onclick={h.cancelDownload}>Cancel Download</button>
    {/if}
</div>
