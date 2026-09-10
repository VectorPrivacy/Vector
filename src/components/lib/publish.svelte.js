// The Nexus publish dialog (trusted publishers): the form's fields, the permission list,
// the App ID hint, and the submit lock. marketplace.js opens it, prefills from an existing
// app, and publishes; the component answers through `handlers`.
const s = $state({
    open: false, active: false, icon: '', busy: false,
    form: { id: '', name: '', description: '', version: '1.0.0', isGame: true, categories: '', developer: '', source: '', changelog: '' },
    perms: [], permsError: '',
    hint: { text: 'Unique identifier (lowercase, no spaces)', accent: false },
});
let handlers = $state.raw(null);   // { cancel(), submit(), idInput() }

export function publishState() { return s; }
export function publishHandlers() { return handlers; }
export function openPublishDialog(form, icon, h) {
    Object.assign(s.form, form); s.icon = icon || ''; s.perms = []; s.permsError = ''; s.busy = false;
    s.hint = { text: 'Unique identifier (lowercase, no spaces)', accent: false };
    s.open = true; s.active = false; handlers = h;
}
export function activatePublishDialog() { s.active = true; }
export function closePublishDialog() { s.active = false; handlers = null; }
export function unmountPublishDialog() { s.open = false; }
export function setPublishPerms(list) { s.perms = list.map(p => ({ ...p, checked: false })); s.permsError = ''; }
export function setPublishPermsError(msg) { s.permsError = msg; }
export function setPublishHint(text, accent) { s.hint = { text, accent: !!accent }; }
export function setPublishBusy(on) { s.busy = !!on; }
