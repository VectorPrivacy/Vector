// The Invites screen: the account's code, or the state of fetching it.
export const invites = $state({ phase: 'loading', code: '', xUrl: '' });   // loading | ok | error
export function setInvites(view) { Object.assign(invites, view); }
