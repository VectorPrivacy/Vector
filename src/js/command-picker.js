/**
 * Slash Command Selector Module
 * Discord-inspired `/` command picker for the chat input, driven by bot
 * manifests (Bot Interface Phase 1).
 *
 * Opens the moment the draft starts with `/`: shows "Loading N bots" while the
 * backend's snapshot/refresh is in flight, then Recently Used on top and one
 * section per bot with its commands in MANIFEST order. Once a command is
 * chosen (or typed exactly), the panel switches to a non-key-consuming hint
 * row with the argument signature; Choice args offer clickable values.
 *
 * Usage:
 *   const ctrl = initCommandSelector(textarea, io, anchorEl);
 *   io = {
 *     load(chatId)      → Promise<{bots, commands, fresh}> backend snapshot
 *     chatId()          → the currently open chat id
 *     accountNpub()     → active account npub (recents are per-account)
 *     botProfile(npub)  → {name, avatarSrc}
 *   }
 *   ctrl.isOpen()                        → list/loading panel consuming keys?
 *   ctrl.routeForSend(text)              → null | {error} | {bot, name}
 *   ctrl.onCommandsUpdated(chatId, snap) → live swap-in from the backend event
 */

// eslint-disable-next-line no-unused-vars
function initCommandSelector(textarea, io, anchorEl) {
    const RECENTS_CAP = 8;

    let mode = 'closed';        // closed | loading | list | hint
    let activeIndex = 0;        // keyboard-highlighted row (list mode)
    let query = '';             // text typed after '/'
    let panel = null;
    let armedPick = null;       // {chatId, bot, name} — the explicitly chosen row
    let hintSuppressedFor = null; // draft value the user Esc'd the hint away for
    const snapshots = new Map(); // chatId → {bots, commands}
    const loading = new Set();   // chatIds with a load() in flight

    function createPanel() {
        const el = document.createElement('div');
        el.className = 'command-selector';
        document.body.appendChild(el);
        return el;
    }
    panel = createPanel();

    function isVisible() { return panel.classList.contains('visible'); }
    function show() { panel.classList.add('visible'); }
    function hide() {
        panel.classList.remove('visible');
        mode = 'closed';
        activeIndex = 0;
    }

    function position() {
        const rect = anchorEl.getBoundingClientRect();
        const margin = 10;
        // Never wider than the viewport itself — small windows and phones.
        const width = Math.min(rect.width, 420, window.innerWidth - margin * 2);
        const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
        panel.style.left = left + 'px';
        panel.style.bottom = (window.innerHeight - rect.top + 6) + 'px';
        panel.style.width = width + 'px';
    }

    // --- Recents (per account, most-recent-first, "<bot>:<name>" keys) ---
    function recentsKey() { return 'vector-cmd-recents:' + (io.accountNpub() || ''); }
    function getRecents() {
        try { return JSON.parse(localStorage.getItem(recentsKey())) || []; } catch (_) { return []; }
    }
    function bumpRecent(bot, name) {
        const key = bot + ':' + name;
        const list = getRecents().filter(k => k !== key);
        list.unshift(key);
        try { localStorage.setItem(recentsKey(), JSON.stringify(list.slice(0, RECENTS_CAP))); } catch (_) {}
    }

    // --- Data ---
    /** Ask the backend for the chat's snapshot. `force` re-asks even when a
     *  memoized snapshot exists — every picker OPEN forces, so a bot that
     *  joined (or got its profile resolved) since the last look is noticed:
     *  the backend compares the bot set per call and refreshes on any change.
     *  Cheap when nothing changed (memory/DB answer, no network). */
    function ensureLoaded(chatId, force) {
        if (loading.has(chatId) || (!force && snapshots.has(chatId))) return;
        loading.add(chatId);
        Promise.resolve(io.load(chatId)).then((snap) => {
            loading.delete(chatId);
            snapshots.set(chatId, snap || { bots: 0, commands: [] });
            if (isVisible() && io.chatId() === chatId) render();
        }).catch(() => {
            loading.delete(chatId);
            if (!snapshots.has(chatId)) snapshots.set(chatId, { bots: 0, commands: [] });
            if (isVisible() && io.chatId() === chatId) render();
        });
    }

    /** Live swap-in: the backend finished its background manifest refresh. */
    function onCommandsUpdated(chatId, snap) {
        snapshots.set(chatId, { bots: snap.bots || 0, commands: snap.commands || [] });
        if (isVisible() && io.chatId() === chatId) render();
    }

    /** Every known command of the open chat: [{bot, name, description, args}]. */
    function allCommands() {
        const snap = snapshots.get(io.chatId());
        const out = [];
        for (const b of (snap && snap.commands) || []) {
            for (const c of b.commands || []) {
                out.push({ bot: b.bot, name: c.name, description: c.description || '', args: c.args || [] });
            }
        }
        return out;
    }

    function findCommand(name, preferBot) {
        const all = allCommands().filter(c => c.name === name);
        if (!all.length) return null;
        if (preferBot) {
            const picked = all.find(c => c.bot === preferBot);
            if (picked) return picked;
        }
        return all[0];
    }

    // --- Shell-style tokenizer (mirrors the Rust invocation parser) ---
    function nextToken(s, i) {
        while (i < s.length && /\s/.test(s[i])) i++;
        if (i >= s.length) return null;
        let out = '';
        if (s[i] === '"') {
            i++;
            while (i < s.length) {
                if (s[i] === '\\' && i + 1 < s.length && (s[i + 1] === '"' || s[i + 1] === '\\')) { out += s[i + 1]; i += 2; }
                else if (s[i] === '"') return { value: out, next: i + 1 };
                else { out += s[i]; i++; }
            }
            return undefined; // unterminated quote
        }
        const start = i;
        while (i < s.length && !/\s/.test(s[i])) i++;
        return { value: s.slice(start, i), next: i };
    }

    /** Positional parse of `rest` (text after the name) against a spec's args.
     *  Mirrors the manifest rules: quoting anywhere, an UNQUOTED trailing
     *  String arg swallows the raw remainder. Returns [{name, value}] or
     *  undefined on an unterminated quote. */
    function parseArgs(spec, rest) {
        const args = [];
        let cursor = 0;
        for (let i = 0; i < spec.args.length; i++) {
            const a = spec.args[i];
            const remainder = rest.slice(cursor).replace(/^\s+/, '');
            if (!remainder) break;
            const isLast = i + 1 === spec.args.length;
            let value;
            if (isLast && a.type === 'string' && !remainder.startsWith('"')) {
                value = remainder.replace(/\s+$/, '');
                cursor = rest.length;
            } else {
                const tok = nextToken(rest, cursor);
                if (tok === undefined) return undefined;
                if (tok === null) break;
                value = tok.value;
                cursor = tok.next;
            }
            args.push({ name: a.name, value });
        }
        return args;
    }

    /** Manifest-type validation. Returns an error string or null when valid. */
    function validateArgs(spec, parsed) {
        const have = new Map(parsed.map(a => [a.name, a.value]));
        for (const a of spec.args) {
            const v = have.get(a.name);
            if (v === undefined) {
                if (a.required) return 'Missing required argument "' + a.name + '"';
                continue;
            }
            switch (a.type) {
                case 'int':
                    if (!/^[+-]?\d+$/.test(v)) return '"' + a.name + '" must be a whole number';
                    break;
                case 'number':
                    if (v.trim() === '' || !isFinite(Number(v))) return '"' + a.name + '" must be a number';
                    break;
                case 'bool':
                    if (!['true', 'false', 'yes', 'no', '1', '0'].includes(v.toLowerCase())) return '"' + a.name + '" must be true or false';
                    break;
                case 'user':
                    if (!v.startsWith('npub1') || v.length > 70) return '"' + a.name + '" must be a user (npub)';
                    break;
                case 'choice':
                    if (!(a.choices || []).includes(v)) return '"' + a.name + '" must be one of: ' + (a.choices || []).join(', ');
                    break;
            }
        }
        return null;
    }

    // --- Rendering ---
    function argSignature(a) {
        return a.required ? '<' + a.name + '>' : '[' + a.name + ']';
    }

    function visibleRows() {
        const q = query.toLowerCase();
        const matches = allCommands().filter(c => c.name.includes(q));
        // Prefix matches first, insertion (manifest) order preserved within each tier.
        matches.sort((x, y) => (y.name.startsWith(q) ? 1 : 0) - (x.name.startsWith(q) ? 1 : 0));
        const recent = [];
        const recentKeys = getRecents();
        for (const key of recentKeys) {
            const sep = key.indexOf(':');
            const hit = matches.find(c => c.bot === key.slice(0, sep) && c.name === key.slice(sep + 1));
            if (hit) recent.push(hit);
        }
        return { recent, matches };
    }

    function commandRow(cmd, flatIndex) {
        const row = document.createElement('div');
        row.className = 'command-item' + (flatIndex === activeIndex ? ' active' : '');
        const name = document.createElement('span');
        name.className = 'command-item-name';
        name.textContent = '/' + cmd.name;
        row.appendChild(name);
        for (const a of cmd.args) {
            const chip = document.createElement('span');
            chip.className = 'command-item-arg' + (a.required ? '' : ' optional');
            chip.textContent = argSignature(a);
            row.appendChild(chip);
        }
        if (cmd.description) {
            const desc = document.createElement('span');
            desc.className = 'command-item-desc';
            desc.textContent = cmd.description;
            row.appendChild(desc);
        }
        row.addEventListener('mousedown', (e) => {
            e.preventDefault(); // keep textarea focus
            selectCommand(cmd);
        });
        return row;
    }

    function sectionHeader(text, avatarSrc) {
        const header = document.createElement('div');
        header.className = 'command-section-header';
        if (avatarSrc) {
            const img = document.createElement('img');
            img.src = avatarSrc;
            img.alt = '';
            header.appendChild(img);
        }
        const label = document.createElement('span');
        label.textContent = text;
        header.appendChild(label);
        return header;
    }

    function render() {
        const chatId = io.chatId();
        const snap = snapshots.get(chatId);
        panel.innerHTML = '';

        // Still fetching and nothing known: the loading state ("Loading N bots").
        if (!snap || (loading.has(chatId) && !allCommands().length)) {
            const n = snap ? snap.bots : 0;
            if (snap && n === 0) { hide(); return; } // known: no bots here
            mode = 'loading';
            const row = document.createElement('div');
            row.className = 'command-loading';
            const spin = document.createElement('span');
            spin.className = 'command-spinner';
            row.appendChild(spin);
            const label = document.createElement('span');
            label.textContent = n > 0 ? ('Loading ' + n + ' bot' + (n === 1 ? '' : 's') + '…') : 'Looking for bots…';
            row.appendChild(label);
            panel.appendChild(row);
            position();
            show();
            return;
        }

        if (snap.bots === 0 || !allCommands().length) { hide(); return; }

        const { recent, matches } = visibleRows();
        if (!matches.length) { hide(); return; }
        mode = 'list';

        // Flat keyboard order = exactly the render order (recents, then sections).
        // Each section wraps its header + rows so the header can STICK to the
        // panel top while its rows scroll, then get pushed away by the next
        // section's header (sticky is clamped to its own wrapper).
        const flat = [];
        const section = (headerText, avatarSrc, cmds) => {
            const wrap = document.createElement('div');
            wrap.className = 'command-section';
            wrap.appendChild(sectionHeader(headerText, avatarSrc));
            for (const cmd of cmds) {
                wrap.appendChild(commandRow(cmd, flat.length));
                flat.push(cmd);
            }
            panel.appendChild(wrap);
        };
        if (recent.length) {
            section('Recently Used', null, recent);
        }
        const byBot = new Map();
        for (const cmd of matches) {
            if (!byBot.has(cmd.bot)) byBot.set(cmd.bot, []);
            byBot.get(cmd.bot).push(cmd);
        }
        for (const [bot, cmds] of byBot) {
            const profile = io.botProfile(bot) || {};
            section(profile.name || (bot.slice(0, 12) + '…'), profile.avatarSrc || null, cmds);
        }
        panel._flat = flat;
        if (activeIndex >= flat.length) activeIndex = 0;
        position();
        show();
        // Rebuilding innerHTML resets scrollTop; bring the keyboard-active row
        // back into view (scroll-margin keeps it clear of the stuck header).
        const activeEl = panel.querySelector('.command-item.active');
        if (activeEl) activeEl.scrollIntoView({ block: 'nearest' });
    }

    /** The armed-command hint bar: signature with the CURRENT arg highlighted;
     *  a Choice arg additionally offers its values as clickable chips. */
    function renderHint(cmd, typedRest) {
        mode = 'hint';
        panel.innerHTML = '';

        // Which arg is the caret conceptually on: completed tokens = args filled.
        let filled = 0;
        let cursor = 0;
        while (filled < cmd.args.length) {
            const tok = nextToken(typedRest, cursor);
            if (tok === null || tok === undefined) break;
            // A token is "completed" once whitespace (or nothing more to type) follows.
            if (tok.next >= typedRest.length && !/\s$/.test(typedRest)) break;
            cursor = tok.next;
            filled++;
        }
        const currentIdx = Math.min(filled, Math.max(cmd.args.length - 1, 0));

        const row = document.createElement('div');
        row.className = 'command-hint';
        const name = document.createElement('span');
        name.className = 'command-item-name';
        name.textContent = '/' + cmd.name;
        row.appendChild(name);
        cmd.args.forEach((a, i) => {
            const chip = document.createElement('span');
            chip.className = 'command-item-arg' + (a.required ? '' : ' optional') + (i === currentIdx && cmd.args.length ? ' current' : '');
            chip.textContent = argSignature(a);
            chip.title = a.description || '';
            row.appendChild(chip);
        });
        panel.appendChild(row);

        const current = cmd.args[currentIdx];
        if (current && current.description) {
            const desc = document.createElement('div');
            desc.className = 'command-hint-desc';
            desc.textContent = current.description;
            panel.appendChild(desc);
        }
        if (current && current.type === 'choice' && (current.choices || []).length) {
            const choices = document.createElement('div');
            choices.className = 'command-hint-choices';
            for (const ch of current.choices) {
                const chip = document.createElement('span');
                chip.className = 'command-choice';
                chip.textContent = ch;
                chip.addEventListener('mousedown', (e) => {
                    e.preventDefault();
                    insertChoice(ch);
                });
                choices.appendChild(chip);
            }
            panel.appendChild(choices);
        }
        position();
        show();
    }

    // --- Selection / insertion ---
    function selectCommand(cmd) {
        armedPick = { chatId: io.chatId(), bot: cmd.bot, name: cmd.name };
        textarea.value = '/' + cmd.name + ' ';
        textarea.selectionStart = textarea.selectionEnd = textarea.value.length;
        activeIndex = 0;
        hintSuppressedFor = null;
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
        textarea.focus();
    }

    function insertChoice(value) {
        const v = textarea.value;
        // Replace a partial trailing token (or append after whitespace) with the value.
        let base = /\s$/.test(v) ? v : v.replace(/\S*$/, '');
        if (!base.startsWith('/')) base = v.endsWith(' ') ? v : v + ' ';
        textarea.value = base + value + ' ';
        textarea.selectionStart = textarea.selectionEnd = textarea.value.length;
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
        textarea.focus();
    }

    // --- Input-driven state machine ---
    function onInput() {
        const val = textarea.value;
        if (!val.startsWith('/')) {
            armedPick = null;
            hintSuppressedFor = null;
            if (isVisible()) hide();
            return;
        }
        if (armedPick && armedPick.chatId !== io.chatId()) armedPick = null;
        if (hintSuppressedFor !== null && hintSuppressedFor !== val) hintSuppressedFor = null;

        // A closed→open transition re-asks the backend (bot set may have grown);
        // keystrokes while already open render from the memo.
        ensureLoaded(io.chatId(), !isVisible());
        const nameEnd = val.search(/\s/);
        if (nameEnd === -1) {
            // Still typing the command name.
            query = val.slice(1);
            activeIndex = 0;
            render();
            return;
        }
        // Past the name: hint the exact command, or get out of the way.
        const name = val.slice(1, nameEnd);
        const cmd = findCommand(name, armedPick && armedPick.name === name ? armedPick.bot : null);
        if (cmd && hintSuppressedFor !== val) {
            renderHint(cmd, val.slice(nameEnd));
        } else if (isVisible()) {
            hide();
        }
    }

    // --- Keyboard navigation (list mode only; the hint never eats keys) ---
    function onKeyDown(e) {
        if (mode === 'hint' && e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            hintSuppressedFor = textarea.value;
            hide();
            return;
        }
        if (mode !== 'list' && mode !== 'loading') return;
        const flat = panel._flat || [];
        if (e.key === 'ArrowDown' && flat.length) {
            e.preventDefault();
            activeIndex = (activeIndex + 1) % flat.length;
            render();
        } else if (e.key === 'ArrowUp' && flat.length) {
            e.preventDefault();
            activeIndex = (activeIndex - 1 + flat.length) % flat.length;
            render();
        } else if ((e.key === 'Enter' || e.key === 'Tab') && flat.length) {
            e.preventDefault();
            e.stopPropagation();
            selectCommand(flat[activeIndex]);
        } else if (e.key === 'Enter' && mode === 'loading') {
            // Don't send a half-formed command into the void while loading.
            e.preventDefault();
            e.stopPropagation();
        } else if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            hide();
        }
    }

    function onBlur() {
        setTimeout(() => { if (isVisible()) hide(); }, 150);
    }

    /**
     * Send-time routing. null = ordinary chat text (unknown command names
     * included — "/shrug" must still send). {error} = a KNOWN command with
     * invalid arguments (block the send, keep the draft). {bot, name} = valid:
     * tag the send to that bot and record the recent.
     */
    function routeForSend(text) {
        if (!text || !text.startsWith('/')) return null;
        const head = nextToken(text.slice(1), 0);
        if (!head || head.value === '' || text[1] === '"') return null;
        const name = head.value;
        const cmd = findCommand(name, armedPick && armedPick.name === name ? armedPick.bot : null);
        if (!cmd) return null;
        const parsed = parseArgs(cmd, text.slice(1 + head.next));
        if (parsed === undefined) return { error: 'Unclosed quote in /' + name };
        const err = validateArgs(cmd, parsed);
        if (err) return { error: '/' + name + ': ' + err };
        bumpRecent(cmd.bot, cmd.name);
        armedPick = null;
        return { bot: cmd.bot, name: cmd.name };
    }

    textarea.addEventListener('input', onInput);
    textarea.addEventListener('keydown', onKeyDown);
    textarea.addEventListener('blur', onBlur);

    return {
        isOpen() { return isVisible() && (mode === 'list' || mode === 'loading'); },
        routeForSend,
        onCommandsUpdated,
        destroy() {
            textarea.removeEventListener('input', onInput);
            textarea.removeEventListener('keydown', onKeyDown);
            textarea.removeEventListener('blur', onBlur);
            if (panel.parentNode) panel.parentNode.removeChild(panel);
        }
    };
}
