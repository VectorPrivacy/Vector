// The policy designer's state: the catalogue (presets and rule kinds), the community's
// stored policies, the channels for exemptions, and the draft being edited. Any edit to
// the draft drops the preview; saving is gated on a preview of THIS draft.
const p = $state({
    communityId: null, view: 'gallery', usingBuiltin: true, previewed: false, busy: false,
    previewError: '', channelsLoaded: false,
});
let presets = $state.raw([]);
let ruleKinds = $state.raw([]);
let stored = $state.raw([]);
let channels = $state.raw([]);
let preview = $state.raw(null);
// Deep: dial edits mutate it in place and every reader moves.
let draft = $state(null);

export function polState() { return p; }
export function polPresets() { return presets; }
export function polRuleKinds() { return ruleKinds; }
export function polStored() { return stored; }
export function polChannels() { return channels; }
export function polPreview() { return preview; }
export function polDraft() { return draft; }

export function polSetCatalogue(nextPresets, nextKinds) { presets = nextPresets || []; ruleKinds = nextKinds || []; }
export function polSetStored(communityId, list, usingBuiltin) {
    p.communityId = communityId; stored = list || []; p.usingBuiltin = !!usingBuiltin;
}
export function polSetChannels(list) { channels = list || []; p.channelsLoaded = true; }
export function polResetChannels() { channels = []; p.channelsLoaded = false; }

export function polShowGallery() { p.view = 'gallery'; }

export function polOpenEditor(preset, policy) {
    draft = {
        id: preset.id, name: preset.name, caveat: preset.caveat, dials: preset.dials || [],
        summary: preset.rules || [], policy, strictness: 'balanced', values: {},
        // A dial seeded from the policy's own patterns REPLACES them on save.
        // Merging would make the box a half-truth: you would edit a domain out
        // and it would come back.
        seeded: false,
    };
    const link = (policy?.rules || []).find(r => r.match?.type === 'link');
    if (preset.id === 'vector_defaults' && link) {
        draft.values.domains = (link.match.patterns || []).join('\n');
        draft.seeded = true;
    }
    if (preset.id === 'blank') { draft.values.name = ''; draft.values.rules = []; }
    // A span dial starts on the shipped rule's own value: an empty box must not mean "instantly".
    const rate = (policy?.rules || []).find(r => r.match?.type === 'rate');
    for (const d of draft.dials) if (d.kind === 'seconds') draft.values[d.key] = String(rate?.match?.per_secs || 10);
    p.previewed = false; p.busy = false; p.previewError = ''; preview = null;
    p.view = 'editor';
}

/** Any edit invalidates the preview: you cannot preview one policy and enable another. */
export function polDirty() { p.previewed = false; p.previewError = ''; preview = null; }
export function polSetBusy(busy) { p.busy = !!busy; }
export function polSetPreview(res) { preview = res; p.previewError = ''; p.previewed = true; p.busy = false; }
export function polSetPreviewError(msg) { preview = null; p.previewError = String(msg); p.previewed = false; p.busy = false; }
