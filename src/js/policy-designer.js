// The Policy Designer: how a community writes its own rules.
//
// One rule shapes everything here — a policy is never enabled from a form
// alone, only from a PREVIEW that named the members it would catch. An
// over-broad rule announces itself by catching regulars, and the fix is a
// button rather than a number. Preview stores nothing and removes nobody.
//
// The state lives in components/lib/policy.svelte.js and the panel is Svelte;
// this side fetches, composes the document, and publishes.

// Resolved at CALL time, not load time: this script is deferred and may run
// before the Tauri bridge is attached.
function polInvoke(cmd, args) { return window.__TAURI__.core.invoke(cmd, args); }

const polDraft = () => VectorSvelte.polDraft();
const polCommunityId = () => VectorSvelte.polState().communityId;

const STRICTNESS = [
    { id: 'relaxed', label: 'Relaxed', mult: 0.8, first: 2,
      says: 'Two hits in one message before it says anything, with lower confidence.' },
    { id: 'balanced', label: 'Balanced', mult: 1.0, first: 1,
      says: 'One hit is enough, at this template\'s own confidence.' },
    { id: 'strict', label: 'Strict', mult: 1.1, first: 1,
      says: 'One hit is enough, with higher confidence.' },
];

const STRICTNESS_EXPLAINER = [
    'Sensitivity sets how much it takes to trip a rule, and how confident the result is.',
    '<b>Relaxed</b>: two hits in one message, with lower confidence.',
    '<b>Balanced</b>: one hit, at the template\'s own confidence.',
    '<b>Strict</b>: one hit, with higher confidence.',
].join('<br><br>');

function polName(npub) {
    // The same resolution the member list uses: a preview that says
    // "npub13l8…" cannot let an admin recognise the people a rule would catch.
    try {
        const p = (arrProfiles || []).find(x => x.id === npub);
        const name = p ? (p.nickname || p.name || p.display_name || '') : '';
        if (name) return name;
    } catch { /* fall through */ }
    return npub.slice(0, 10) + '…' + npub.slice(-4);
}

let polMounted = false;
function polEnsureMounted() {
    if (polMounted) return;
    polMounted = true;
    VectorSvelte.mountPolicyDesigner(modPoliciesPane, {
        h: {
            strictness: STRICTNESS,
            parse: polParse,
            name: polName,
            readiness: polReadiness,
            explainStrictness: () => popupConfirm('Sensitivity', STRICTNESS_EXPLAINER, true),
            inspectDefaults: () => {
                const d = VectorSvelte.polPresets().find(p => p.id === 'vector_defaults');
                if (d) polOpenEditor(d);
            },
            open: polOpenEditor,
            preview: polPreview,
            save: polSave,
            toggle: async (p) => {
                await polInvoke('set_community_policy', {
                    communityId: polCommunityId(), policyId: p.policy_id, bytes: p.bytes, enabled: !p.enabled,
                });
                await polRefresh();
            },
            remove: async (id) => {
                await polInvoke('delete_community_policy', { communityId: polCommunityId(), policyId: id });
                await polRefresh();
            },
        },
    });
}

async function openPolicyDesigner(communityId) {
    polEnsureMounted();
    if (!VectorSvelte.polPresets().length) {
        const r = await polInvoke('policy_presets');
        VectorSvelte.polSetCatalogue(r.presets || [], r.rule_kinds || []);
    }
    await polRefresh(communityId);
    VectorSvelte.polShowGallery();
}

async function polRefresh(communityId = polCommunityId()) {
    const r = await polInvoke('list_community_policies', { communityId });
    VectorSvelte.polSetStored(communityId, r.policies || [], r.using_builtin);
}

function polOpenEditor(preset) {
    VectorSvelte.polOpenEditor(preset, polParse(preset.bytes));
}

/// Clamp a span to the range the validator accepts, falling back to whatever the
/// rule shipped with rather than to zero — an empty box must not mean "instantly".
function polSeconds(value, fallback) {
    const n = parseInt(value, 10);
    if (!Number.isFinite(n)) return fallback;
    return Math.max(1, Math.min(3600, n));
}

/// The span a fresh `seconds` dial starts on: the shipped rule's own value.
function polSecondsDefault() {
    const rule = (polDraft()?.policy?.rules || []).find(r => r.match?.type === 'rate');
    return String(rule?.match?.per_secs || 10);
}

/// Fold the dials into the policy document the engine will evaluate.
function polCompose() {
    const draft = polDraft();
    const doc = JSON.parse(JSON.stringify(draft.policy));
    const s = STRICTNESS.find(x => x.id === draft.strictness) || STRICTNESS[1];
    const lines = (key) => (draft.values[key] || '')
        .split(/[\n,]/).map(x => x.trim()).filter(Boolean);
    const split = (text) => (text || '').split(/[\n,]/).map(x => x.trim()).filter(Boolean);

    if (draft.values.name) doc.name = draft.values.name.slice(0, 64);

    // A template's span dial writes onto whichever rule counts within one.
    if (draft.values.per_secs) {
        for (const rule of doc.rules) {
            if (rule.match?.type === 'rate') {
                rule.match.per_secs = polSeconds(draft.values.per_secs, rule.match.per_secs);
            }
        }
    }

    // From-scratch policies carry no rules until the author adds them. The
    // template rule comes from core so the weights and rungs are the same
    // numbers the presets ship, never a set the UI invented.
    if (draft.dials.some(d => d.kind === 'rules')) {
        const seen = new Set();
        doc.rules = (draft.values.rules || []).map((r, i) => {
            const kind = VectorSvelte.polRuleKinds().find(k => k.id === r.kind);
            if (!kind) return null;
            const built = JSON.parse(JSON.stringify(kind.rule));
            // Rule ids must be unique within a policy or the document is inert.
            let id = built.id;
            while (seen.has(id)) id = `${built.id}-${i}`;
            seen.add(id);
            built.id = id;
            if (kind.input === 'seconds') built.match.per_secs = polSeconds(r.value, built.match.per_secs);
            else if (kind.input !== 'none') built.match.patterns = split(r.value);
            return built;
        }).filter(Boolean);
    }

    for (const rule of doc.rules) {
        // Strictness scales weights and how soon the first rung fires. Capped
        // at 99: 100 is reserved, and the validator rejects it.
        if (rule.tiers) {
            for (const scope of ['per_message', 'per_window']) {
                (rule.tiers[scope] || []).forEach((rung, i) => {
                    rung.weight = Math.max(1, Math.min(99, Math.round(rung.weight * s.mult)));
                    if (i === 0 && scope === 'per_message') rung.hits = s.first;
                });
            }
        }
        if (rule.match.type === 'keyword') {
            const words = lines('words');
            if (words.length) rule.match.patterns = words;
        }
        if (rule.match.type === 'link') {
            const extra = lines('domains');
            // Seeded means the box was pre-filled with this rule's own list, so
            // it is the whole truth: deleting a line has to delete the domain.
            if (draft.seeded) rule.match.patterns = extra;
            else if (extra.length) rule.match.patterns = [...new Set([...(rule.match.patterns || []), ...extra])];
            const allow = lines('allow');
            if (allow.length) rule.exempt = { ...(rule.exempt || {}), patterns: [{ kind: 'domain', values: allow }] };
        }
        const chans = draft.values.exempt_channels || [];
        if (chans.length) rule.exempt = { ...(rule.exempt || {}), channels: chans };
    }
    return doc;
}

/// Whether the draft is worth running, and why not if it isn't.
///
/// Judged on the COMPOSED policy, never on whether the form fields have text in
/// them: Scam Links carries its bundled shortener list inside the rule, so both
/// of its boxes are legitimately empty and the policy still catches things.
function polReadiness() {
    let doc;
    try {
        doc = polCompose();
    } catch {
        return { ok: false, reason: 'This policy is not ready yet.' };
    }
    if (!doc.rules.length) {
        return { ok: false, reason: 'Add at least one rule before previewing.' };
    }
    // An empty keyword rule matches nothing; an empty LINK rule matches every
    // link there is. Opposite failures, both from a blank box, and neither is
    // something to hand to a preview.
    const bare = doc.rules.find(r => ['keyword', 'link'].includes(r.match.type) && !(r.match.patterns || []).length);
    if (bare) {
        return {
            ok: false,
            reason: bare.match.type === 'link'
                ? 'One of your rules has no domains in it.'
                : 'One of your rules has no words in it.',
        };
    }
    return { ok: true, reason: '' };
}

async function polPreview() {
    // Belt: the button is disabled, but a stray call must not preview a policy
    // that cannot say anything.
    const gate = polReadiness();
    if (!gate.ok) { VectorSvelte.polSetPreviewError(gate.reason); return; }
    const doc = polCompose();
    VectorSvelte.polSetBusy(true);
    let res;
    try {
        res = await polInvoke('preview_community_policy', {
            communityId: polCommunityId(), bytes: JSON.stringify(doc),
        });
    } catch (e) {
        VectorSvelte.polSetPreviewError(String(e));
        return;
    }
    if (!res.valid) { VectorSvelte.polSetPreviewError(res.error || 'This policy is not valid.'); return; }
    VectorSvelte.polSetPreview(res);
}

/// A stable, readable id per saved policy. `blank` is the id of the TEMPLATE,
/// so saving under it would let the second from-scratch policy overwrite the
/// first.
function polNewId(name) {
    const base = (name || 'policy').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'policy';
    const taken = new Set(VectorSvelte.polStored().map(p => p.policy_id));
    if (!taken.has(base)) return base;
    for (let i = 2; i < 500; i++) if (!taken.has(`${base}-${i}`)) return `${base}-${i}`;
    return base;
}

async function polSave() {
    if (!VectorSvelte.polState().previewed) return; // structural: the button is disabled, this is the belt
    const doc = polCompose();
    const draft = polDraft();
    const id = draft.id === 'blank' ? polNewId(doc.name) : draft.id;
    await polInvoke('set_community_policy', {
        communityId: polCommunityId(), policyId: id, bytes: JSON.stringify(doc), enabled: true,
    });
    await polRefresh();
    VectorSvelte.polShowGallery();
}

function polParse(bytes) {
    try { return JSON.parse(bytes); } catch { return null; }
}

// Plain script, like the rest of the console: expose the entry point the
// moderation panel calls.
window.openPolicyDesigner = openPolicyDesigner;
