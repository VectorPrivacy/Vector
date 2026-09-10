<script>
    // The composer's attachment panel: the main buttons, the mini apps view (search + grid)
    // and the PIVX wallet card. Renders inside the fixed container the composer positions
    // and toggles `visible` on; the wallet's border tint rides the container too.
    import { attachmentState, setAttachmentRootEl } from '../lib/attachmentpanel.svelte.js';
    import { gridState } from '../lib/miniappsgrid.svelte.js';
    import MiniAppsGrid from './MiniAppsGrid.svelte';
    import PivxWallet from './pivx/PivxWallet.svelte';

    let { h } = $props();   // h: AttachmentPanelHelpers (js/miniapps-panel.js)
    //    search(q), bindGrid(el) → cleanup, grid: MiniAppsGrid's bag, pivx: PivxWallet's bag
    const st = attachmentState();
    function rootEl(node) { setAttachmentRootEl(node); return { destroy() { setAttachmentRootEl(null); } }; }

    // Staggered fade-in of a view's items, replayed whenever its pulse moves.
    function stagger(node, pulse) {
        const run = () => {
            const items = node.querySelectorAll('.attachment-panel-item');
            const step = items.length > 1 ? Math.min(0.08, 0.35 / (items.length - 1)) : 0;
            items.forEach((item, i) => {
                item.classList.remove('animate-in');
                item.style.animationDelay = '';
                void item.offsetWidth;
                item.style.animationDelay = `${i * step}s`;
                item.classList.add('animate-in');
                item.addEventListener('animationend', () => {
                    item.classList.remove('animate-in');
                    item.style.animationDelay = '';
                }, { once: true });
            });
        };
        run();
        return { update: run };
    }
    function grid(node) { return { destroy: h.bindGrid(node) }; }
</script>

<div class="attachment-panel" id="attachment-panel" tabindex="-1" use:rootEl class:visible={st.visible} class:pivx-active={st.view === 'pivx'} style:bottom={st.bottom || null}>

{#if st.view === 'main'}
    <div class="attachment-panel-content" id="attachment-panel-main" use:stagger={st.tick.main}>
        <button class="attachment-panel-item" id="attachment-panel-file" onclick={h.file}>
            <div class="attachment-panel-btn"><span class="icon icon-file"></span></div>
            <span class="attachment-panel-label">File</span>
        </button>
        {#if st.folderShown}
            <button class="attachment-panel-item" id="attachment-panel-folder" onclick={h.folder}>
                <div class="attachment-panel-btn"><span class="icon icon-folder"></span></div>
                <span class="attachment-panel-label">Folder</span>
            </button>
        {/if}
        {#if st.commandsShown}
            <button class="attachment-panel-item" id="attachment-panel-commands" class:disabled={st.commandsDisabled}
                    onclick={h.commands} onmouseenter={(e) => h.commandsEnter(e.currentTarget)} onmouseleave={h.commandsLeave}>
                <div class="attachment-panel-btn">
                    <svg class="attachment-panel-svg" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M7 22L17 2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                </div>
                <span class="attachment-panel-label">Commands</span>
            </button>
        {/if}
        <button class="attachment-panel-item" id="attachment-panel-miniapps" onclick={h.miniapps}>
            <div class="attachment-panel-btn"><span class="icon icon-gamepad"></span></div>
            <span class="attachment-panel-label">Mini Apps</span>
        </button>
    </div>
{:else if st.view === 'miniapps'}
    <div class="attachment-panel-content" id="attachment-panel-miniapps-view">
        <div class="miniapps-topbar">
            <button class="miniapps-back-btn" id="attachment-panel-back" onclick={h.back}>
                <span class="icon icon-chevron-double-left"></span>
                <span>Back</span>
            </button>
            <div class="miniapps-search-container">
                <span class="icon icon-search"></span>
                <input type="text" class="miniapps-search-input" id="miniapps-search" placeholder="Search"
                       autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                       value={st.search} oninput={(e) => h.search(e.currentTarget.value)}>
            </div>
        </div>
        <div class="miniapps-grid" id="miniapps-grid" class:edit-mode={gridState().editMode} use:grid use:stagger={st.tick.grid}>
            <MiniAppsGrid h={h.grid} />
        </div>
    </div>
{:else}
    <div class="attachment-panel-content pivx-wallet-panel" id="attachment-panel-pivx-view">
        <PivxWallet h={h.pivx} pulse={st.tick.pivx} />
    </div>
{/if}
</div>
