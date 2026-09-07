// The pack details modal: which pack it shows and what the fetch said.
let details = $state.raw(null);   // null (closed) | { naddr, state: 'loading' | 'ok' | 'err', pack?, error? }

export function packDetails() { return details; }
export function openPackDetails(naddr) { details = { naddr, state: 'loading' }; }
export function resolvePackDetails(naddr, result) {
    if (details?.naddr === naddr) details = { naddr, ...result };
}
export function closePackDetails() { details = null; }
