// The widescreen Start Menu: whether it is the navigation in use, whether it is open,
// and the pins it shows (the rail's shape: nodes of items and folders, keys are pin
// keys). js/startmenu.js owns the commands and resolves what each key shows.

const st = $state({
    // 'start' (the menu) or 'navbar' (the rail's own tabs); per device.
    mode: 'start',
    open: false,
    // Where the menu grows from: your row's box and its avatar's, when it opened.
    anchor: null,
    // On screen, from opening until it has shrunk back into the row.
    visible: false,
    layout: { v: 1, nodes: [] },
    // Moves when something a tile shows may have changed: a chat, an app, a name.
    seq: 0,
});

export function startMenuState() { return st; }
export function setStartMode(mode) { st.mode = mode === 'navbar' ? 'navbar' : 'start'; if (st.mode !== 'start') st.open = false; }
export function setStartLayout(layout) { st.layout = layout && Array.isArray(layout.nodes) ? layout : { v: 1, nodes: [] }; st.seq++; }
export function setStartOpen(open, anchor = null) {
    st.open = !!open && st.mode === 'start';
    if (st.open) {
        if (anchor) st.anchor = anchor;
        st.seq++;
    }
}
export function setStartVisible(on) { st.visible = !!on; }
