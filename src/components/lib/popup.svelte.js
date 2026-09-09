// The app's one confirm/notice popup: what it shows and the pending answer. `popupConfirm`
// (js/misc.js) opens it and owns the promise; the component reports confirm or cancel.
const p = $state({
    open: false, title: '', html: '', notice: false, placeholder: '', icon: '', circular: false,
    titleClass: '', confirmText: 'Confirm', value: '', actions: null,
});
let answer = null;   // { confirm(), cancel() } for the open popup

export function popupState() { return p; }
export function openPopupDialog(view, handlers) {
    Object.assign(p, { actions: null }, view, { open: true, value: '' });
    answer = handlers;
}
export function closePopupDialog() { p.open = false; answer = null; }
export function popupAnswer() { return answer; }
