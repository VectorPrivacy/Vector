// The Policy Designer: how a community writes its own rules.
//
// One rule shapes everything here — a policy is never enabled from a form
// alone, only from a PREVIEW that named the members it would catch. An
// over-broad rule announces itself by catching regulars, and the fix is a
// button rather than a number. Preview stores nothing and removes nobody.

// Resolved at CALL time, not load time: this script is deferred and may run
// before the Tauri bridge is attached, and grabbing it early would throw and
// take every export with it.
function polInvoke(cmd, args) { return window.__TAURI__.core.invoke(cmd, args); }

let polCommunityId = null;
let polPresets = [];
let polStored = [];
let polDraft = null;      // { id, name, caveat, dials, policy }
let polPreviewed = false; // the gate: saving needs a preview of THIS draft

const STRICTNESS = [
    { id: 'relaxed', label: 'Relaxed', mult: 0.8, first: 2 },
    { id: 'balanced', label: 'Balanced', mult: 1.0, first: 1 },
    { id: 'strict', label: 'Strict', mult: 1.1, first: 1 },
];

function polEl(id) { return document.getElementById(id); }

function polName(npub) {
    // The same resolution the member list uses — a preview that says
    // "npub13l8…" cannot do its job, which is to let an admin recognise the
    // people a rule would catch.
    try {
        const p = (arrProfiles || []).find(x => x.id === npub);
        const name = p ? (p.nickname || p.name || p.display_name || '') : '';
        if (name) return name;
    } catch { /* fall through */ }
    return npub.slice(0, 10) + '…' + npub.slice(-4);
}

async function openPolicyDesigner(communityId) {
    polCommunityId = communityId;
    if (!polPresets.length) {
        const r = await polInvoke('policy_presets');
        polPresets = r.presets || [];
    }
    await polRefresh();
    polShowGallery();
}

async function polRefresh() {
    const r = await polInvoke('list_community_policies', { communityId: polCommunityId });
    polStored = r.policies || [];
    polRenderList(r.using_builtin);
}

function polRenderList(usingBuiltin) {
    const list = polEl('pol-list');
    if (!list) return;
    if (!polStored.length) {
        list.innerHTML = `<div class="pol-empty">No policies yet — this community runs Vector's built-in raid and scam-link rules.</div>`;
        return;
    }
    list.innerHTML = '';
    for (const p of polStored) {
        const doc = polParse(p.bytes);
        const row = document.createElement('div');
        row.className = 'pol-row';
        const rules = doc ? doc.rules.length : 0;
        row.innerHTML = `
            <div class="pol-row-main">
                <div class="pol-row-name">${polEscape(doc?.name || p.policy_id)}</div>
                <div class="pol-row-sub ${p.valid ? '' : 'pol-row-invalid'}">
                    ${p.valid ? `${rules} rule${rules === 1 ? '' : 's'} · ${p.enabled ? 'active' : 'paused'}`
                              : `Not running: ${polEscape(String(p.error || 'invalid'))}`}
                </div>
            </div>
            <button class="pol-toggle ${p.enabled ? 'on' : ''}" data-id="${polEscape(p.policy_id)}" aria-label="Toggle"></button>
            <button class="pol-delete" data-id="${polEscape(p.policy_id)}" aria-label="Delete">&#x2715;</button>`;
        list.appendChild(row);
    }
    list.querySelectorAll('.pol-toggle').forEach(b => {
        b.onclick = async () => {
            const p = polStored.find(x => x.policy_id === b.dataset.id);
            if (!p) return;
            await polInvoke('set_community_policy', {
                communityId: polCommunityId, policyId: p.policy_id, bytes: p.bytes, enabled: !p.enabled,
            });
            await polRefresh();
        };
    });
    list.querySelectorAll('.pol-delete').forEach(b => {
        b.onclick = async () => {
            await polInvoke('delete_community_policy', { communityId: polCommunityId, policyId: b.dataset.id });
            await polRefresh();
        };
    });
    void usingBuiltin;
}

function polShowGallery() {
    polEl('pol-editor').style.display = 'none';
    polEl('pol-gallery').style.display = '';
    polEl('pol-list').style.display = '';
    const g = polEl('pol-gallery');
    g.innerHTML = '';
    for (const p of polPresets) {
        const card = document.createElement('button');
        card.className = 'pol-card';
        card.innerHTML = `
            <div class="pol-card-name">${polEscape(p.name)}</div>
            <div class="pol-card-desc">${polEscape(p.description)}</div>
            <div class="pol-card-eg">Catches: ${polEscape(p.example)}</div>`;
        card.onclick = () => polOpenEditor(p);
        g.appendChild(card);
    }
}

function polOpenEditor(preset) {
    polDraft = {
        id: preset.id,
        name: preset.name,
        caveat: preset.caveat,
        dials: preset.dials || [],
        policy: polParse(preset.bytes),
        strictness: 'balanced',
        values: {},
    };
    polPreviewed = false;
    polEl('pol-gallery').style.display = 'none';
    polEl('pol-list').style.display = 'none';
    polEl('pol-editor').style.display = '';
    polEl('pol-editor-title').textContent = preset.name;
    polEl('pol-editor-caveat').textContent = preset.caveat;
    polEl('pol-preview-out').style.display = 'none';
    polSetSaveEnabled(false);
    polRenderDials();
}

function polRenderDials() {
    const wrap = polEl('pol-dials');
    wrap.innerHTML = '';
    for (const d of polDraft.dials) {
        const box = document.createElement('div');
        box.className = 'pol-dial';
        box.innerHTML = `<label class="pol-dial-label">${polEscape(d.label)}</label>
                         <div class="pol-dial-hint">${polEscape(d.hint)}</div>`;
        if (d.kind === 'strictness') {
            const seg = document.createElement('div');
            seg.className = 'pol-seg';
            for (const s of STRICTNESS) {
                const b = document.createElement('button');
                b.className = 'pol-seg-btn' + (polDraft.strictness === s.id ? ' active' : '');
                b.textContent = s.label;
                b.onclick = () => { polDraft.strictness = s.id; polDirty(); polRenderDials(); };
                seg.appendChild(b);
            }
            box.appendChild(seg);
        } else if (d.kind === 'wordlist' || d.kind === 'domainlist') {
            const ta = document.createElement('textarea');
            ta.rows = 4;
            ta.value = polDraft.values[d.key] || '';
            ta.placeholder = d.kind === 'wordlist' ? 'one word per line' : 'example.com';
            ta.oninput = () => { polDraft.values[d.key] = ta.value; polDirty(); };
            box.appendChild(ta);
        } else if (d.kind === 'channels') {
            const chips = document.createElement('div');
            chips.className = 'pol-chips';
            const chosen = new Set(polDraft.values[d.key] || []);
            for (const ch of (window.polChannels || [])) {
                const c = document.createElement('button');
                c.className = 'pol-chip' + (chosen.has(ch.id) ? ' on' : '');
                c.textContent = '#' + ch.name;
                c.onclick = () => {
                    chosen.has(ch.id) ? chosen.delete(ch.id) : chosen.add(ch.id);
                    polDraft.values[d.key] = [...chosen];
                    polDirty();
                    polRenderDials();
                };
                chips.appendChild(c);
            }
            if (!chips.children.length) chips.innerHTML = '<span class="pol-hint">No channels found.</span>';
            box.appendChild(chips);
        }
        wrap.appendChild(box);
    }
}

/// Any edit invalidates the preview: you cannot preview one policy and enable
/// a different one.
function polDirty() {
    polPreviewed = false;
    polSetSaveEnabled(false);
    polEl('pol-preview-out').style.display = 'none';
    polEl('pol-preview-hint').textContent = 'Changed — preview again before enabling.';
}

function polSetSaveEnabled(on) {
    const b = polEl('pol-save');
    if (b) b.disabled = !on;
}

/// Fold the dials into the policy document the engine will evaluate.
function polCompose() {
    const doc = JSON.parse(JSON.stringify(polDraft.policy));
    const s = STRICTNESS.find(x => x.id === polDraft.strictness) || STRICTNESS[1];
    const lines = (key) => (polDraft.values[key] || '')
        .split(/[\n,]/).map(x => x.trim()).filter(Boolean);

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
            if (extra.length) rule.match.patterns = [...new Set([...(rule.match.patterns || []), ...extra])];
            const allow = lines('allow');
            if (allow.length) rule.exempt = { ...(rule.exempt || {}), patterns: [{ kind: 'domain', values: allow }] };
        }
        const chans = polDraft.values.exempt_channels || [];
        if (chans.length) rule.exempt = { ...(rule.exempt || {}), channels: chans };
    }
    return doc;
}

async function polPreview() {
    const doc = polCompose();
    // A keyword rule with no words matches nothing — say so rather than
    // previewing an empty result that looks like safety.
    const empty = doc.rules.some(r => r.match.type === 'keyword' && !(r.match.patterns || []).length);
    if (empty) {
        polShowPreviewError('Add at least one word before previewing.');
        return;
    }
    polEl('pol-preview-hint').textContent = 'Checking against real history…';
    let res;
    try {
        res = await polInvoke('preview_community_policy', {
            communityId: polCommunityId, bytes: JSON.stringify(doc),
        });
    } catch (e) {
        polShowPreviewError(String(e));
        return;
    }
    if (!res.valid) {
        polShowPreviewError(res.error || 'This policy is not valid.');
        return;
    }
    polRenderPreview(res);
    polPreviewed = true;
    polSetSaveEnabled(true);
    polEl('pol-preview-hint').textContent = 'Previewed. Nothing has been changed yet.';
}

function polShowPreviewError(msg) {
    const out = polEl('pol-preview-out');
    out.style.display = '';
    out.innerHTML = `<div class="pol-warn"><span class="pol-warn-name">Not ready:</span> ${polEscape(msg)}</div>`;
    polPreviewed = false;
    polSetSaveEnabled(false);
}

function polRenderPreview(res) {
    const out = polEl('pol-preview-out');
    out.style.display = '';
    const n = res.flagged.length;
    const days = 7;
    let html = `<div class="pol-preview-head">
        Over the last ${days} days this would have flagged
        <span class="pol-preview-num">${n}</span> member${n === 1 ? '' : 's'}
        and cited <span class="pol-preview-num">${res.messages_cited}</span> message${res.messages_cited === 1 ? '' : 's'}.
    </div>`;
    if (!n) {
        html += `<div class="pol-hint">Nothing in your history trips this rule. That is usually good — and it also means
                 this preview cannot tell you whether the rule works. Try a word you know someone has used.</div>`;
    }
    // The number the flagged list cannot show you: regulars who tripped the
    // same wire and were spared only by their standing. A short flagged list
    // can hide a rule that catches ordinary conversation.
    if (res.shielded_matches.length) {
        const names = res.shielded_matches.slice(0, 4).map(r => `<span class="pol-warn-name">${polEscape(polName(r.npub))}</span>
            (${r.messages} msgs)`).join(', ');
        html += `<div class="pol-warn">⚠️ ${res.shielded_matches.length} trusted member${res.shielded_matches.length === 1 ? '' : 's'} also matched this rule and
                 ${res.shielded_matches.length === 1 ? 'was' : 'were'} spared only by their standing: ${names}${res.shielded_matches.length > 4 ? ', …' : ''}.
                 If this rule is meant for raiders, it is catching ordinary conversation.</div>`;
    }
    if (res.unevaluated.length) {
        html += `<div class="pol-hint" style="margin-top:8px;">Not checked here: ${polEscape(res.unevaluated.join(', '))}</div>`;
    }
    if (n) {
        html += '<div class="pol-preview-rows">';
        for (const r of res.flagged.slice(0, 30)) {
            html += `<div class="pol-preview-row">
                <span class="pol-preview-row-name">${polEscape(polName(r.npub))}</span>
                <span class="pol-preview-row-why">${polEscape((r.reasons || [])[0] || '')}</span>
                <span class="pol-preview-score">${r.score}${r.proven === 0 ? ' · suspected' : ' · provable'}</span>
            </div>`;
        }
        html += '</div>';
    }
    out.innerHTML = html;
}

async function polSave() {
    if (!polPreviewed) return; // structural: the button is disabled, this is the belt
    const doc = polCompose();
    const id = polDraft.id;
    await polInvoke('set_community_policy', {
        communityId: polCommunityId, policyId: id, bytes: JSON.stringify(doc), enabled: true,
    });
    await polRefresh();
    polShowGallery();
}

function polParse(bytes) {
    try { return JSON.parse(bytes); } catch { return null; }
}

function polEscape(s) {
    return String(s ?? '').replace(/[&<>"']/g, c =>
        ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function initPolicyDesigner() {
    polEl('pol-back')?.addEventListener('click', polShowGallery);
    polEl('pol-preview')?.addEventListener('click', polPreview);
    polEl('pol-save')?.addEventListener('click', polSave);
}

// Plain script, like the rest of the console: expose the two entry points the
// moderation panel calls.
window.openPolicyDesigner = openPolicyDesigner;
window.initPolicyDesigner = initPolicyDesigner;
document.addEventListener('DOMContentLoaded', initPolicyDesigner);
