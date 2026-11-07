<script module>
    /** A label's id in a link to it (`settings/appearance/display/composer`). */
    export const groupId = (label) => label.toLowerCase().replace(/[^a-z0-9]+/g, '-');

    // `slim` / `wide`: only that layout has the row's subject.
    const ROWS = [
        { key: 'startMenu', id: 'start-menu-toggle', label: 'Main Menu', group: 'Navigation', wide: true },
        { key: 'hideRail', id: 'hide-rail-toggle', label: 'Hide Community Rail', group: 'Chat List', slim: true },
        { key: 'imageTypes', id: 'display-image-types-toggle', label: 'Display Image Types', group: 'Chat' },
        { key: 'chatBg', id: 'chat-bg-toggle', label: 'Background Wallpaper', group: 'Chat' },
        { key: 'emoticons', id: 'emoticon-suggestions-toggle', label: 'Emoticon Suggestions', group: 'Composer' },
        { key: 'timeSuggestions', id: 'time-suggestions-toggle', label: 'Time Suggestions', group: 'Composer' },
        { key: 'autocorrect', id: 'autocorrect-toggle', label: 'Autocorrect', group: 'Composer' },
        { key: 'sendOnEnter', id: 'send-on-enter-toggle', label: 'Send on Enter', group: 'Composer' },
        { key: 'floatingPlayer', id: 'floating-player-toggle', label: 'Floating Player', group: 'Media' },
    ];
    const rowsFor = (ws) => ROWS.filter((r) => (ws ? !r.slim : !r.wide));

    /** The group labels this layout shows, in order: the lead's (the theme) first. */
    export const displayGroups = (ws) => ['Appearance', ...new Set(rowsFor(ws).map((r) => r.group))];

    /** Each row this layout shows as `[label, group label]`, the lead's included. */
    export const displayRows = (ws) => [['Theme', 'Appearance'], ...rowsFor(ws).map((r) => [r.label, r.group])];
</script>

<script>
    // The Display section: toggle rows derived from display state. Each change
    // hands the new value to the app, which persists and applies it.
    import { displayState } from '../lib/settings.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    // grouped: a label above each run of rows that share a group. lead: the Appearance
    // group's rows (the theme picker), owned by the screen.
    let { h, grouped = false, lead = null } = $props();   // h: DisplayHelpers (js/settings.js)

    const d = displayState();
    const shell = shellState();
    const rows = $derived(rowsFor(shell.ws));
    function info(key) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(key); };
    }
</script>

{#if lead}
    {#if grouped}<p class="st-group section-label" data-group="appearance">Appearance</p>{/if}
    {@render lead()}
{/if}
{#each rows as r, i (r.key)}
    {#if grouped && r.group !== rows[i - 1]?.group}<p class="st-group section-label" data-group={groupId(r.group)}>{r.group}</p>{/if}
    <div class="form-group">
        <label class="toggle-container">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <span><span class="icon icon-info btn notif-info" onclick={info(r.key)}></span>{r.label}</span>
            <input type="checkbox" id={r.id} checked={d[r.key]} onchange={(e) => { d[r.key] = e.target.checked; h.change(r.key, d[r.key]); }}>
            <span class="neon-toggle"></span>
        </label>
    </div>
{/each}
