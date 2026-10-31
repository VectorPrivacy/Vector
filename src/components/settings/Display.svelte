<script>
    // The Display section: toggle rows derived from display state. Each change
    // hands the new value to the app, which persists and applies it.
    import { displayState } from '../lib/settings.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    // grouped: a faint label above each run of rows that share a group.
    let { h, grouped = false } = $props();   // h: DisplayHelpers (js/settings.js)

    const d = displayState();
    const shell = shellState();
    // `slim`: only the slim layout has the row's subject.
    const ROWS = [
        { key: 'hideRail', id: 'hide-rail-toggle', label: 'Hide Community Rail', group: 'Chat List', slim: true },
        { key: 'imageTypes', id: 'display-image-types-toggle', label: 'Display Image Types', group: 'Chat' },
        { key: 'chatBg', id: 'chat-bg-toggle', label: 'Background Wallpaper', group: 'Chat' },
        { key: 'emoticons', id: 'emoticon-suggestions-toggle', label: 'Emoticon Suggestions', group: 'Composer' },
        { key: 'timeSuggestions', id: 'time-suggestions-toggle', label: 'Time Suggestions', group: 'Composer' },
        { key: 'autocorrect', id: 'autocorrect-toggle', label: 'Autocorrect', group: 'Composer' },
        { key: 'sendOnEnter', id: 'send-on-enter-toggle', label: 'Send on Enter', group: 'Composer' },
        { key: 'floatingPlayer', id: 'floating-player-toggle', label: 'Floating Player', group: 'Media' },
    ];
    const rows = $derived(shell.ws ? ROWS.filter((r) => !r.slim) : ROWS);
    function info(key) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(key); };
    }
</script>

{#each rows as r, i (r.key)}
    {#if grouped && r.group !== rows[i - 1]?.group}<p class="st-group">{r.group}</p>{/if}
    <div class="form-group">
        <label class="toggle-container">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <span><span class="icon icon-info btn notif-info" onclick={info(r.key)}></span>{r.label}</span>
            <input type="checkbox" id={r.id} checked={d[r.key]} onchange={(e) => { d[r.key] = e.target.checked; h.change(r.key, d[r.key]); }}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/each}
