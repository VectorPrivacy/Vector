// The channel editor: a channel's name, topic and view, drafted until Save.

const s = $state({ open: false, communityId: null, channelId: null, name: '', topic: '', view: 'chat', busy: false, error: '' });

export function channelEditorState() { return s; }

/** Open on `channel` ({ id, name, topic, view }) of `communityId`. */
export function openChannelEditor(communityId, channel) {
    Object.assign(s, {
        open: true, communityId, channelId: channel.id, name: channel.name || '', topic: channel.topic || '',
        view: channel.view || 'chat', busy: false, error: '',
    });
}
export function closeChannelEditor() { s.open = false; s.busy = false; s.error = ''; }
export function setChannelEditorBusy(on, error = '') { s.busy = !!on; s.error = error; }
