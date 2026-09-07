// The hover toolbar over a message row: which actions apply to the row under the
// cursor, and the delete button's flavour. The app decides; the buttons derive.
let tb = $state.raw({
    show: {},            // { react, reply, edit, reveal, copy, retry, cancel, delete }: true = offered
    path: null,          // the downloaded attachment behind reveal / copy
    del: null,           // { mode: 'delete' | 'hide' | 'failed', label, partial, hasAttachments }
});
export function messageToolbar() { return tb; }
export function setMessageToolbar(view) { tb = view; }
