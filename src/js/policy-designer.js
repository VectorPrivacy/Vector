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
let polRuleKinds = [];
let polStored = [];
let polDraft = null;      // { id, name, caveat, dials, policy }
let polPreviewed = false; // the gate: saving needs a preview of THIS draft

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
        polRuleKinds = r.rule_kinds || [];
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
    // The shipped defaults run alongside anything the community writes, so they
    // are shown as a row rather than as the absence of one. When the community
    // has forked them the row says who replaced them: a badge that claims cover
    // the engine is not providing is worse than no badge.
    list.innerHTML = usingBuiltin
        ? `<div class="pol-row pol-row-builtin">
               <div class="pol-row-main">
                   <div class="pol-row-name">Vector's defaults</div>
                   <div class="pol-row-sub">Watching for raid waves &middot; running</div>
               </div>
               <button class="pol-inspect" id="pol-inspect">See the rules</button>
           </div>`
        : `<div class="pol-row pol-row-builtin pol-row-forked">
               <div class="pol-row-main">
                   <div class="pol-row-name">Vector's defaults</div>
                   <div class="pol-row-sub">Replaced by your own version below.</div>
               </div>
           </div>`;
    const inspect = polEl('pol-inspect');
    if (inspect) {
        inspect.onclick = () => {
            const d = polPresets.find(p => p.id === 'vector_defaults');
            if (d) polOpenEditor(d);
        };
    }
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
}

function polShowGallery() {
    polEl('pol-editor').style.display = 'none';
    polEl('pol-gallery').style.display = '';
    polEl('pol-list').style.display = '';
    polEl('pol-scratch').style.display = '';
    const g = polEl('pol-gallery');
    g.innerHTML = '';
    // "Start from scratch" answers a different question from the templates, so
    // it gets its own row rather than sitting in the lineup pretending to be
    // one more ready-made answer.
    // The defaults are already on screen as the row above, whose "See the rules"
    // opens this same editor. A card for them too would be a second handle on
    // one door, sitting an inch from the first.
    const shown = polPresets.filter(x => x.id !== 'blank' && x.id !== 'vector_defaults');
    for (const p of shown) {
        const card = document.createElement('button');
        card.className = 'pol-card';
        card.innerHTML = `
            <div class="pol-card-name">${polEscape(p.name)}</div>
            <div class="pol-card-desc">${polEscape(p.description)}</div>
            <div class="pol-card-eg">Catches: ${polEscape(p.example)}</div>`;
        card.onclick = () => polOpenEditor(p);
        g.appendChild(card);
    }
    const blank = polPresets.find(x => x.id === 'blank');
    const scratch = polEl('pol-scratch');
    if (scratch) {
        scratch.innerHTML = '';
        if (blank) {
            const b = document.createElement('button');
            b.className = 'pol-scratch-btn';
            b.innerHTML = `<span class="pol-scratch-plus">+</span>
                <span class="pol-scratch-text">
                    <span class="pol-scratch-name">${polEscape(blank.name)}</span>
                    <span class="pol-scratch-desc">${polEscape(blank.description)}</span>
                </span>`;
            b.onclick = () => polOpenEditor(blank);
            scratch.appendChild(b);
        }
    }
}

function polOpenEditor(preset) {
    const policy = polParse(preset.bytes);
    polDraft = {
        id: preset.id,
        name: preset.name,
        caveat: preset.caveat,
        dials: preset.dials || [],
        summary: preset.rules || [],
        policy,
        strictness: 'balanced',
        values: {},
        // A dial seeded from the policy's own patterns REPLACES them on save.
        // Merging would make the box a half-truth: you would edit a domain out
        // and it would come back.
        seeded: false,
    };
    // Seed a domain box from the rule's own list where one exists, so what you
    // see is the whole list rather than an empty box beside a hidden one.
    const link = (policy?.rules || []).find(r => r.match?.type === 'link');
    if (preset.id === 'vector_defaults' && link) {
        polDraft.values.domains = (link.match.patterns || []).join('\n');
        polDraft.seeded = true;
    }
    if (preset.id === 'blank') {
        polDraft.values.name = '';
        polDraft.values.rules = [];
    }
    polPreviewed = false;
    polEl('pol-gallery').style.display = 'none';
    polEl('pol-list').style.display = 'none';
    polEl('pol-scratch').style.display = 'none';
    polEl('pol-editor').style.display = '';
    polEl('pol-editor-title').textContent = preset.name;
    polEl('pol-editor-caveat').textContent = preset.caveat;
    polEl('pol-preview-out').style.display = 'none';
    polPreviewState('idle');
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
            // The app's own info affordance: a question with a real answer
            // behind it beats a label nobody can act on.
            const info = document.createElement('span');
            info.className = 'icon icon-info pol-info';
            info.setAttribute('role', 'button');
            info.setAttribute('aria-label', 'What does this change?');
            info.onclick = (e) => {
                e.preventDefault();
                e.stopPropagation();
                popupConfirm('Sensitivity', STRICTNESS_EXPLAINER, true);
            };
            box.querySelector('.pol-dial-label').appendChild(info);
        }
        if (d.kind === 'summary') {
            const list = document.createElement('div');
            list.className = 'pol-summary';
            for (const r of polDraft.summary) {
                const item = document.createElement('div');
                item.className = 'pol-sum' + (r.armed ? ' pol-sum-armed' : '');
                item.innerHTML = `<div class="pol-sum-name">${polEscape(r.label)}${
                    r.armed ? '<span class="pol-sum-tag">only after another rule fires</span>' : ''
                }</div><div class="pol-sum-detail">${polEscape(r.detail)}</div>`;
                list.appendChild(item);
            }
            box.appendChild(list);
        } else if (d.kind === 'strictness') {
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
            const says = document.createElement('div');
            says.className = 'pol-seg-says';
            says.textContent = (STRICTNESS.find(x => x.id === polDraft.strictness) || STRICTNESS[1]).says;
            box.appendChild(says);
        } else if (d.kind === 'wordlist' || d.kind === 'domainlist') {
            const ta = document.createElement('textarea');
            ta.rows = 4;
            ta.value = polDraft.values[d.key] || '';
            ta.placeholder = d.kind === 'wordlist' ? 'one word per line' : 'example.com';
            ta.oninput = () => { polDraft.values[d.key] = ta.value; polDirty(); };
            box.appendChild(ta);
        } else if (d.kind === 'text') {
            const inp = document.createElement('input');
            inp.type = 'text';
            inp.className = 'pol-text';
            inp.value = polDraft.values[d.key] || '';
            inp.placeholder = 'Spoiler filter';
            inp.oninput = () => { polDraft.values[d.key] = inp.value; polDirty(); };
            box.appendChild(inp);
        } else if (d.kind === 'rules') {
            box.appendChild(polBuildRuleList());
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
            // The channel list arrives with the console's intel read, which can
            // still be in flight when the editor opens. Saying "none" then is
            // wrong, and it never corrects itself.
            if (!chips.children.length) {
                chips.innerHTML = window.polChannelsLoaded
                    ? '<span class="pol-hint">This community has no channels.</span>'
                    : '<span class="pol-hint">Loading channels…</span>';
            }
            box.appendChild(chips);
        }
        wrap.appendChild(box);
    }
    polRefreshPreviewGate();
}

/// The from-scratch rule list. Every kind on offer can convict on its own —
/// the catalogue withholds the aggravators, which mean nothing unarmed.
function polBuildRuleList() {
    const wrap = document.createElement('div');
    wrap.className = 'pol-rules';
    const rules = polDraft.values.rules || [];

    rules.forEach((r, i) => {
        const kind = polRuleKinds.find(k => k.id === r.kind);
        if (!kind) return;
        const row = document.createElement('div');
        row.className = 'pol-rule';
        row.innerHTML = `
            <div class="pol-rule-head">
                <span class="pol-rule-name">${polEscape(kind.label)}</span>
                <button class="pol-rule-x" aria-label="Remove">&#x2715;</button>
            </div>
            <div class="pol-rule-desc">${polEscape(kind.description)}</div>`;
        if (kind.input !== 'none') {
            const ta = document.createElement('textarea');
            ta.rows = 3;
            ta.value = r.value || '';
            ta.placeholder = kind.input === 'wordlist' ? 'one word per line' : 'example.com';
            ta.oninput = () => { r.value = ta.value; polDirty(); };
            const lab = document.createElement('div');
            lab.className = 'pol-rule-hint';
            lab.textContent = kind.input_hint;
            row.appendChild(lab);
            row.appendChild(ta);
        }
        row.querySelector('.pol-rule-x').onclick = () => {
            rules.splice(i, 1);
            polDirty();
            polRenderDials();
        };
        wrap.appendChild(row);
    });

    const add = document.createElement('div');
    add.className = 'pol-rule-add';
    for (const k of polRuleKinds) {
        const b = document.createElement('button');
        b.className = 'pol-rule-add-btn';
        b.textContent = '+ ' + k.label;
        b.title = k.description;
        b.onclick = () => {
            rules.push({ kind: k.id, value: '' });
            polDraft.values.rules = rules;
            polDirty();
            polRenderDials();
        };
        add.appendChild(b);
    }
    wrap.appendChild(add);
    return wrap;
}

/// Called once the console's intel read lands, so an editor opened before the
/// channels arrived stops claiming there are none.
window.polChannelsReady = function polChannelsReady() {
    if (polEl('pol-editor')?.style.display !== 'none') polRenderDials();
};

/// The preview button carries its own state: a spinner and what it is doing
/// while it reads, then straight back to the invitation. The result itself is
/// the panel below and the Enable button unlocking, so the button has nothing
/// left to announce once it is done.
function polPreviewState(state) {
    const b = polEl('pol-preview');
    if (!b) return;
    const busy = state === 'busy';
    b.classList.toggle('is-busy', busy);
    b.disabled = busy;
    b.querySelector('.mod-btn-label').textContent = busy ? 'Checking real history…' : 'Preview on real history';
    if (!busy) polRefreshPreviewGate();
}

/// Any edit invalidates the preview: you cannot preview one policy and enable
/// a different one.
function polDirty() {
    polPreviewed = false;
    polSetSaveEnabled(false);
    polEl('pol-preview-out').style.display = 'none';
    polPreviewState('idle');
    polRefreshPreviewGate();
}

function polSetSaveEnabled(on) {
    const b = polEl('pol-save');
    if (!b) return;
    b.disabled = !on;
    // The locked button names its own key. "Enable this policy", greyed out,
    // leaves the reader hunting for why; this says what to do about it, and the
    // preview sitting beside it is the thing to do.
    b.querySelector('.mod-btn-label').textContent = on ? 'Enable this policy' : 'Preview before Enabling';
}

/// Fold the dials into the policy document the engine will evaluate.
function polCompose() {
    const doc = JSON.parse(JSON.stringify(polDraft.policy));
    const s = STRICTNESS.find(x => x.id === polDraft.strictness) || STRICTNESS[1];
    const lines = (key) => (polDraft.values[key] || '')
        .split(/[\n,]/).map(x => x.trim()).filter(Boolean);
    const split = (text) => (text || '').split(/[\n,]/).map(x => x.trim()).filter(Boolean);

    if (polDraft.values.name) doc.name = polDraft.values.name.slice(0, 64);

    // From-scratch policies carry no rules until the author adds them. The
    // template rule comes from core so the weights and rungs are the same
    // numbers the presets ship, never a set the UI invented.
    if (polDraft.dials.some(d => d.kind === 'rules')) {
        const seen = new Set();
        doc.rules = (polDraft.values.rules || []).map((r, i) => {
            const kind = polRuleKinds.find(k => k.id === r.kind);
            if (!kind) return null;
            const built = JSON.parse(JSON.stringify(kind.rule));
            // Rule ids must be unique within a policy or the document is inert.
            let id = built.id;
            while (seen.has(id)) id = `${built.id}-${i}`;
            seen.add(id);
            built.id = id;
            const values = split(r.value);
            if (kind.input !== 'none') built.match.patterns = values;
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
            if (polDraft.seeded) rule.match.patterns = extra;
            else if (extra.length) rule.match.patterns = [...new Set([...(rule.match.patterns || []), ...extra])];
            const allow = lines('allow');
            if (allow.length) rule.exempt = { ...(rule.exempt || {}), patterns: [{ kind: 'domain', values: allow }] };
        }
        const chans = polDraft.values.exempt_channels || [];
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

/// Dim Preview whenever running it would be pointless or dangerous, so the
/// button stops offering something it would only refuse.
function polRefreshPreviewGate() {
    const b = polEl('pol-preview');
    if (!b || b.classList.contains('is-busy')) return;
    const { ok, reason } = polReadiness();
    b.disabled = !ok;
    b.title = ok ? '' : reason;
}

async function polPreview() {
    // Belt: the button is disabled, but a stray call must not preview a policy
    // that cannot say anything.
    const gate = polReadiness();
    if (!gate.ok) {
        polShowPreviewError(gate.reason);
        return;
    }
    const doc = polCompose();
    polPreviewState('busy');
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
    polPreviewState('idle');
}

function polShowPreviewError(msg) {
    // Whatever went wrong, the button must not stay spinning: the error panel
    // below it is the report, and the button goes back to being an offer.
    polPreviewState('idle');
    const out = polEl('pol-preview-out');
    out.style.display = '';
    out.innerHTML = `<div class="pol-warn"><span class="pol-warn-name">Not ready:</span> ${polEscape(msg)}</div>`;
    polPreviewed = false;
    polSetSaveEnabled(false);
}

function polNum(n) { return (n || 0).toLocaleString(); }

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
        // Naming the corpus separates "your rule caught nobody" from "there was
        // nothing here to catch" — without it a zero reads the same either way,
        // and an empty history looks exactly like a broken rule.
        html += `<div class="pol-hint">Checked against ${polNum(res.corpus)} message${res.corpus === 1 ? '' : 's'},
                 and nothing tripped this rule. Usually good news, though it also means the preview cannot tell you
                 whether the rule works. Try a word you know someone has used.</div>`;
    }
    // The number the flagged list cannot show you: regulars who tripped the
    // same wire and were spared only by their standing. A short flagged list
    // can hide a rule that catches ordinary conversation.
    if (res.shielded_matches.length) {
        const names = res.shielded_matches.slice(0, 4).map(r => `<span class="pol-warn-name">${polEscape(polName(r.npub))}</span>
            (${r.messages} msgs)`).join(', ');
        html += `<div class="pol-warn">${res.shielded_matches.length} trusted member${res.shielded_matches.length === 1 ? '' : 's'} also matched this rule and
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

/// A stable, readable id per saved policy. `blank` is the id of the TEMPLATE,
/// so saving under it would let the second from-scratch policy overwrite the
/// first.
function polNewId(name) {
    const base = (name || 'policy').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'policy';
    const taken = new Set(polStored.map(p => p.policy_id));
    if (!taken.has(base)) return base;
    for (let i = 2; i < 500; i++) if (!taken.has(`${base}-${i}`)) return `${base}-${i}`;
    return base;
}

async function polSave() {
    if (!polPreviewed) return; // structural: the button is disabled, this is the belt
    const doc = polCompose();
    const id = polDraft.id === 'blank' ? polNewId(doc.name) : polDraft.id;
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
