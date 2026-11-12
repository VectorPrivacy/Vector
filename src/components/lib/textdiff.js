// Word-level diffs between two revisions of a message, folded to what changed.

/**
 * Words, the whitespace between them, and sentence punctuation and markdown delimiters on their
 * own, so "kindness." to "kindness and patience." is an insertion and a changed bold word keeps
 * its `**`. A `:shortcode:` stays one token.
 */
export function tokenize(text) {
    return text ? text.match(/:[a-zA-Z0-9_~-]+:|\s+|[.,!?;]+|[*~`|]+|[^\s.,!?;*~`|]+/g) : [];
}

const isWord = (t) => /[\p{L}\p{N}]/u.test(t);

// Past this many cells the middle is taken as replaced whole: the table stays bounded however
// long both versions are, and a rewrite that large reads better whole anyway.
const MAX_CELLS = 4_000_000;

/**
 * The edit from `before` to `after` as runs: `{ op: 'eq' | 'del' | 'ins', text }`. A common
 * head and tail are matched first, the rest by longest common subsequence of tokens.
 */
export function diffWords(before, after) {
    const a = tokenize(before), b = tokenize(after);
    let head = 0;
    while (head < a.length && head < b.length && a[head] === b[head]) head++;
    let tail = 0;
    while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
    const am = a.slice(head, a.length - tail), bm = b.slice(head, b.length - tail);
    const toks = [];
    const push = (op, tok) => toks.push({ op, tok });
    for (let i = 0; i < head; i++) push('eq', a[i]);
    if (am.length * bm.length > MAX_CELLS) {
        for (const t of am) push('del', t);
        for (const t of bm) push('ins', t);
    } else {
        const n = am.length, m = bm.length, w = m + 1;
        const lcs = new Uint32Array((n + 1) * w);
        for (let i = n - 1; i >= 0; i--) {
            for (let j = m - 1; j >= 0; j--) {
                lcs[i * w + j] = am[i] === bm[j] ? lcs[(i + 1) * w + j + 1] + 1 : Math.max(lcs[(i + 1) * w + j], lcs[i * w + j + 1]);
            }
        }
        let i = 0, j = 0;
        while (i < n || j < m) {
            if (i < n && j < m && am[i] === bm[j]) { push('eq', am[i]); i++; j++; }
            else if (j < m && (i >= n || lcs[i * w + j + 1] >= lcs[(i + 1) * w + j])) push('ins', bm[j++]);
            else push('del', am[i++]);
        }
    }
    for (let i = a.length - tail; i < a.length; i++) push('eq', a[i]);
    slideRight(toks);
    const ops = [];
    for (const { op, tok } of toks) addTo(ops, op, tok);
    return absorbSpaces(ops);
}

/**
 * Push each pure insertion or deletion as far right as the text allows: of the equal ways to
 * place "x." beside a kept ".", the one that keeps the earlier copy reads as the edit.
 */
function slideRight(toks) {
    let s = 0;
    while (s < toks.length) {
        if (toks[s].op === 'eq') { s++; continue; }
        const op = toks[s].op;
        let e = s;
        while (e < toks.length && toks[e].op === op) e++;
        if (e < toks.length && toks[e].op !== 'eq') { s = e; continue; }
        while (e < toks.length && toks[e].op === 'eq' && toks[e].tok === toks[s].tok) {
            toks[s].op = 'eq';
            toks[e].op = op;
            s++; e++;
        }
        s = e;
    }
}

/**
 * A space alone between two changes reads as part of them, not as a kept island, and each
 * change shows as everything it removed, then everything it added.
 */
function absorbSpaces(ops) {
    const out = [];
    let del = '', ins = '', open = false;
    const flush = () => {
        // Whitespace both sides share at either end was never changed.
        let lead = 0;
        while (lead < del.length && lead < ins.length && del[lead] === ins[lead] && /\s/.test(del[lead])) lead++;
        let trail = 0;
        while (trail < del.length - lead && trail < ins.length - lead
            && del[del.length - 1 - trail] === ins[ins.length - 1 - trail] && /\s/.test(del[del.length - 1 - trail])) trail++;
        if (lead) addTo(out, 'eq', del.slice(0, lead));
        const d = del.slice(lead, del.length - trail), n = ins.slice(lead, ins.length - trail);
        if (d) out.push({ op: 'del', text: d });
        if (n) out.push({ op: 'ins', text: n });
        if (trail) out.push({ op: 'eq', text: del.slice(del.length - trail) });
        del = ''; ins = ''; open = false;
    };
    ops.forEach((r, k) => {
        const lone = r.op === 'eq' && /^\s+$/.test(r.text) && open && ops[k + 1] && ops[k + 1].op !== 'eq';
        if (r.op === 'eq' && !lone) {
            flush();
            addTo(out, 'eq', r.text);
            return;
        }
        open = true;
        if (r.op !== 'ins') del += r.text;
        if (r.op !== 'del') ins += r.text;
    });
    flush();
    return out;
}

function addTo(list, op, text) {
    const last = list[list.length - 1];
    if (last && last.op === op) last.text += text;
    else list.push({ op, text });
}

/**
 * Fold the unchanged stretches of `ops` down to the lines a change sits on: each changed line
 * shows whole up to `line` words (else `context` words either side), and the lines between fold
 * to `{ op: 'gap', words, lines }`. Lines stay whole so markdown on them still parses.
 */
export function foldUnchanged(ops, context = 8, line = 40) {
    const out = [];
    const words = (toks) => toks.filter(isWord).length;
    ops.forEach((r, k) => {
        if (r.op !== 'eq') { out.push(r); return; }
        const toks = tokenize(r.text);
        const take = (from, count, dir) => {
            let seen = 0, idx = from;
            while (idx >= 0 && idx < toks.length && seen < count) {
                if (isWord(toks[idx])) seen++;
                idx += dir;
            }
            return idx;
        };
        // After a change: the rest of its line. Before one: the start of its line.
        let headEnd = 0;
        if (k > 0) {
            const nl = toks.findIndex((t) => t.includes('\n'));
            const lineEnd = nl === -1 ? toks.length : nl;
            headEnd = words(toks.slice(0, lineEnd)) <= line ? lineEnd : take(0, context, 1);
        }
        let tailStart = toks.length;
        if (k < ops.length - 1) {
            let nl = -1;
            for (let i = toks.length - 1; i >= 0; i--) if (toks[i].includes('\n')) { nl = i; break; }
            tailStart = words(toks.slice(nl + 1)) <= line ? nl + 1 : take(toks.length - 1, context, -1) + 1;
        }
        const hidden = toks.slice(headEnd, Math.max(headEnd, tailStart));
        const hiddenWords = words(hidden);
        const lines = hidden.some((t) => t.includes('\n'));
        if (headEnd >= tailStart || hiddenWords === 0 || (hiddenWords <= 3 && !lines)) { out.push(r); return; }
        if (headEnd) out.push({ op: 'eq', text: toks.slice(0, headEnd).join('') });
        out.push({ op: 'gap', words: hiddenWords, lines });
        if (tailStart < toks.length) out.push({ op: 'eq', text: toks.slice(tailStart).join('') });
    });
    return out;
}

/** Words added and removed by `ops`. */
export function changeCounts(ops) {
    let added = 0, removed = 0;
    for (const r of ops) {
        const n = tokenize(r.text || '').filter(isWord).length;
        if (r.op === 'ins') added += n;
        else if (r.op === 'del') removed += n;
    }
    return { added, removed };
}
