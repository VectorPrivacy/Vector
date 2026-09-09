// The shared toast: one line of text over a bottom gradient. Auto-hides on a length-scaled
// timer unless persisted, in which case hideToast() ends it.
const t = $state({ text: '', visible: false });
let timer = null;
export function toastState() { return t; }
export function showToast(message, persist = false) {
    t.text = message;
    t.visible = true;
    clearTimeout(timer);
    if (persist) return;
    // 1.5s base plus 40ms per character, capped at 6s.
    timer = setTimeout(() => { t.visible = false; }, Math.min(1500 + message.length * 40, 6000));
}
export function hideToast() { clearTimeout(timer); t.visible = false; }
