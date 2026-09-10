<script>
    // The Display section: toggle rows derived from display state. Each change
    // hands the new value to the app, which persists and applies it.
    import { displayState } from '../lib/settings.svelte.js';
    let { h } = $props();   // h: change(key, value), explain(key)

    const d = displayState();
    const rows = [
        { key: 'imageTypes', id: 'display-image-types-toggle', label: 'Display Image Types' },
        { key: 'chatBg', id: 'chat-bg-toggle', label: 'Background Wallpaper' },
        { key: 'richComposer', id: 'rich-composer-toggle', label: 'Rich Composer' },
        { key: 'emoticons', id: 'emoticon-suggestions-toggle', label: 'Emoticon Suggestions' },
        { key: 'autocorrect', id: 'autocorrect-toggle', label: 'Autocorrect' },
    ];
    function info(key) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(key); };
    }
</script>

{#each rows as r (r.key)}
    <div class="form-group">
        <label class="toggle-container">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <span><span class="icon icon-info btn notif-info" onclick={info(r.key)}></span>{r.label}</span>
            <input type="checkbox" id={r.id} checked={d[r.key]} onchange={(e) => { d[r.key] = e.target.checked; h.change(r.key, d[r.key]); }}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/each}
