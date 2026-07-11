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
        snapshots.set(chatId, { bots: snap.bots || 0, commands: snap.commands || [], fresh: true });
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

    /** One value against one arg spec. Returns an error suffix or null. */
    function argTypeError(a, v) {
        // Wire cap: the manifest parser drops any longer value, which would
        // silently demote the whole invocation to ordinary chat on the bot side.
        if (v.length > 1024) return 'is too long (max 1024 characters)';
        switch (a.type) {
            case 'int':
                if (!/^[+-]?\d+$/.test(v)) return 'must be a whole number';
                break;
            case 'number':
                if (v.trim() === '' || !isFinite(Number(v))) return 'must be a number';
                break;
            case 'bool':
                if (!['true', 'false', 'yes', 'no', '1', '0'].includes(v.toLowerCase())) return 'must be true or false';
                break;
            case 'user':
                if (!v.startsWith('npub1') || v.length > 70) return 'must be a user (npub)';
                break;
            case 'choice':
                if (!(a.choices || []).includes(v)) return 'must be one of: ' + (a.choices || []).join(', ');
                break;
        }
        return null;
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
            const err = argTypeError(a, v);
            if (err) return '"' + a.name + '" ' + err;
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
        // A stale-served list is still converging (manifest REQ in flight) —
        // say so, so a bot that pops in seconds later isn't a surprise.
        if (snap.fresh === false) {
            const row = document.createElement('div');
            row.className = 'command-loading command-refreshing';
            const spin = document.createElement('span');
            spin.className = 'command-spinner';
            row.appendChild(spin);
            const label = document.createElement('span');
            label.textContent = 'Checking for new commands…';
            row.appendChild(label);
            panel.appendChild(row);
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

    // --- Selection → the structured command composer ---
    // Picking a command with args swaps the textarea for one typed input per
    // argument (quoting/escaping is code's job, never the user's), while the
    // reply-bar-style strip above shows "Using /cmd with Bot". Param-less
    // commands send instantly. Typing manually stays the plain-text path.
    let composing = null; // { cmd, chatId, bar, parts: [{arg, el}] }

    // The context strip (a sibling of the reply bar, same tuck design).
    const ctxBar = {
        cmd: document.getElementById('chat-command-bar-cmd'),
        bot: document.getElementById('chat-command-bar-bot'),
        cancel: document.getElementById('chat-command-bar-cancel')
    };
    if (ctxBar.cancel) ctxBar.cancel.addEventListener('click', () => exitComposer(false));

    function showContextBar(cmd) {
        if (!ctxBar.cmd) return;
        ctxBar.cmd.textContent = '/' + cmd.name;
        ctxBar.bot.innerHTML = '';
        const profile = io.botProfile(cmd.bot) || {};
        if (profile.avatarSrc) {
            const img = document.createElement('img');
            img.src = profile.avatarSrc;
            img.alt = '';
            ctxBar.bot.appendChild(img);
        }
        const name = document.createElement('span');
        name.textContent = profile.name || cmd.bot.slice(0, 12) + '…';
        ctxBar.bot.appendChild(name);
        anchorEl.classList.add('commanding');
    }

    function selectCommand(cmd) {
        hintSuppressedFor = null;
        activeIndex = 0;
        hide();
        enterCommandMode(cmd);
    }

    function isComposing() {
        return composing !== null;
    }

    function exitComposer(keepPick) {
        anchorEl.classList.remove('commanding');
        closeChoiceMenu();
        if (!composing) return;
        // Keep the Android back stack in sync when we close via our own paths
        // (Esc, cancel, send, chat switch); no-op after a hardware back pop.
        popBack('command-composer');
        composing.bar.remove();
        textarea.style.display = '';
        composing = null;
        if (!keepPick) armedPick = null;
        io.composerToggled(false);
        textarea.focus();
    }

    function enterCommandMode(cmd) {
        exitComposer(true);
        armedPick = { chatId: io.chatId(), bot: cmd.bot, name: cmd.name };

        // Nothing to fill: the selection IS the send.
        if (!cmd.args.length) {
            textarea.value = '';
            io.submit('/' + cmd.name);
            return;
        }

        const bar = document.createElement('div');
        bar.className = 'command-composer';

        const parts = [];
        for (const a of cmd.args) {
            const wrap = document.createElement('label');
            wrap.className = 'command-part' + (a.required ? ' required' : '');
            const tag = document.createElement('span');
            tag.className = 'command-part-name';
            tag.textContent = a.name;
            wrap.appendChild(tag);
            let el;
            if (a.type === 'choice' || a.type === 'bool') {
                // A custom trigger + drop-up, NOT a native select: those render
                // inconsistently per platform (Android opens an OS modal) and
                // can't match the composer's keyboard flow. The button quacks
                // like a field (.value, focus, empty-backspace walking).
                el = document.createElement('button');
                el.type = 'button';
                el.classList.add('command-choice-trigger');
                el.dataset.placeholder = a.required ? 'choose…' : '(skip)';
                el.value = '';
                const label = document.createElement('span');
                label.textContent = el.dataset.placeholder;
                el.classList.add('placeholder');
                el.appendChild(label);
                el.addEventListener('mousedown', (e) => {
                    e.preventDefault();
                    el.focus();
                    // `idx` is this part's slot, declared just below (closure
                    // runs long after initialization).
                    if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
                    else openChoiceMenu(el, idx, a);
                });
                el.addEventListener('blur', () => {
                    setTimeout(() => {
                        if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
                    }, 120);
                });
            } else if (a.type === 'string') {
                // Free text can be arbitrarily long: a 1-row textarea grows WIDE
                // with its content until the composer's width stops it, then
                // wraps and grows DOWN (scrollHeight) — the label stays pinned
                // at the pill's top once it goes multi-line.
                el = document.createElement('textarea');
                el.rows = 1;
                el.autocomplete = 'off';
                el.spellcheck = true;
            } else {
                el = document.createElement('input');
                el.type = 'text';
                if (a.type === 'int' || a.type === 'number') el.inputMode = 'decimal';
                if (a.type === 'user') el.placeholder = 'npub1…';
                el.autocomplete = 'off';
                el.spellcheck = false;
            }
            el.className = 'command-part-input';
            el.title = a.description || '';
            const idx = parts.length;
            // The trailing free-text arg (the greedy tail on the wire) starts
            // at the rest of its row; other fields grow with their content
            // (JS-sized: field-sizing isn't in WKWebView).
            const grows = a.type === 'string' && idx === cmd.args.length - 1;
            if (grows) wrap.classList.add('grow');
            const autoSize = () => {
                // Width = MEASURED text pixels (ch-guessing undershoots on
                // wide glyphs like m/w and wraps too early) + padding + caret
                // slack; border-box, so the CSS max-width still does the
                // wide-then-wrap clamp.
                if (el.tagName === 'TEXTAREA') {
                    if (!grows) el.style.width = Math.max(72, Math.ceil(measureFieldText(el)) + 26) + 'px';
                    el.style.height = 'auto';
                    el.style.height = el.scrollHeight + 'px';
                    wrap.classList.toggle('multiline', el.offsetHeight > 30);
                } else if (el.tagName === 'INPUT') {
                    el.style.width = Math.min(300, Math.max(72, Math.ceil(measureFieldText(el)) + 26)) + 'px';
                }
            };
            if (el.tagName === 'BUTTON') {
                el.addEventListener('keydown', (e) => onChoiceKey(e, idx, a));
            } else if (a.type === 'user') {
                el.addEventListener('keydown', (e) => onUserKey(e, idx));
                el.addEventListener('focus', () => openUserMenu(el, idx));
                el.addEventListener('input', () => openUserMenu(el, idx));
                el.addEventListener('blur', () => {
                    setTimeout(() => {
                        if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
                    }, 120);
                });
            } else {
                el.addEventListener('keydown', (e) => onPartKey(e, idx));
            }
            el.addEventListener('input', () => {
                wrap.classList.remove('invalid');
                autoSize();
            });
            el.addEventListener('change', () => wrap.classList.remove('invalid'));
            wrap.appendChild(el);
            bar.appendChild(wrap);
            parts.push({ arg: a, el, autoSize });
        }
        textarea.value = '';
        textarea.style.display = 'none';
        textarea.parentElement.insertBefore(bar, textarea);
        // Size the fields once mounted (scrollHeight needs layout): a fresh
        // rows=1 textarea is otherwise UA-default tall, floating its text high.
        for (const p of parts) p.autoSize();
        composing = { cmd, chatId: io.chatId(), bar, parts };
        showContextBar(cmd);
        io.composerToggled(true);
        // Android hardware back closes the composer first, like Esc on desktop.
        pushBack('command-composer', () => exitComposer(false));
        parts[0].el.focus();
    }

    function onPartKey(e, idx) {
        const parts = composing ? composing.parts : [];
        if (e.key === 'Enter') {
            // Shift+Enter in a free-text param is a literal newline (the wire
            // format carries them fine — quoted values span lines).
            if (e.shiftKey && e.target.tagName === 'TEXTAREA') return;
            e.preventDefault();
            e.stopPropagation();
            // Enter advances; on the last part (or with Cmd/Ctrl) it sends.
            if (e.metaKey || e.ctrlKey || idx === parts.length - 1) submitComposer();
            else parts[idx + 1].el.focus();
        } else if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            exitComposer();
        } else if ((e.key === 'Backspace' || e.key === 'Delete') && !e.target.value) {
            // Deleting through an empty part walks backwards: caret lands at
            // the END of the prior value, and walking past the first part
            // cancels the whole command (the keyboard-only escape hatch).
            e.preventDefault();
            if (idx === 0) {
                exitComposer();
                return;
            }
            focusPart(idx - 1, 'end');
        } else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
            // Arrowing past a param's edge crosses into the bordering param:
            // Right at the end lands at the START of the next, Left at the
            // start lands at the END of the prior. Choice triggers have no
            // caret, so either arrow crosses from them.
            const el = e.target;
            const caretFree = el.tagName === 'BUTTON';
            const hasSelection = !caretFree && el.selectionStart !== el.selectionEnd;
            if (e.key === 'ArrowRight' && idx < parts.length - 1
                && (caretFree || (!hasSelection && el.selectionEnd === el.value.length))) {
                e.preventDefault();
                focusPart(idx + 1, 'start');
            } else if (e.key === 'ArrowLeft' && idx > 0
                && (caretFree || (!hasSelection && el.selectionStart === 0))) {
                e.preventDefault();
                focusPart(idx - 1, 'end');
            }
        }
    }

    /** Measure a field's current text (or placeholder) in its own font. */
    let _measureCtx = null;
    function measureFieldText(el) {
        if (!_measureCtx) _measureCtx = document.createElement('canvas').getContext('2d');
        _measureCtx.font = getComputedStyle(el).font;
        return _measureCtx.measureText(el.value || el.placeholder || '').width;
    }

    // ── The Choice drop-up (one floating menu, reused per trigger) ──────────
    let choiceMenu = null;
    let choiceOpenFor = null; // { el: trigger, idx, options: [{v, label}], active }

    function ensureChoiceMenu() {
        if (!choiceMenu) {
            choiceMenu = document.createElement('div');
            choiceMenu.className = 'command-choice-menu';
            document.body.appendChild(choiceMenu);
        }
        return choiceMenu;
    }

    function openChoiceMenu(trigger, idx, arg) {
        const options = [];
        if (!arg.required) options.push({ v: '', label: '(skip)' });
        for (const c of (arg.type === 'bool' ? ['true', 'false'] : arg.choices || [])) {
            options.push({ v: c, label: c });
        }
        if (!options.length) return;
        const active = Math.max(0, options.findIndex(o => o.v === trigger.value));
        choiceOpenFor = { el: trigger, idx, options, active };
        renderChoiceMenu();
    }

    function renderChoiceMenu() {
        if (!choiceOpenFor) return;
        const { el, options, active } = choiceOpenFor;
        const menu = ensureChoiceMenu();
        menu.innerHTML = '';
        for (let i = 0; i < options.length; i++) {
            const row = document.createElement('div');
            row.className = 'command-choice-option'
                + (i === active ? ' active' : '')
                + (options[i].v === '' ? ' skip' : '');
            if (options[i].avatarSrc) {
                const img = document.createElement('img');
                img.src = options[i].avatarSrc;
                img.alt = '';
                row.appendChild(img);
            }
            const label = document.createElement('span');
            label.textContent = options[i].label;
            row.appendChild(label);
            const v = options[i].v;
            row.addEventListener('mousedown', (ev) => {
                ev.preventDefault();
                pickChoice(v);
            });
            menu.appendChild(row);
        }
        const r = el.getBoundingClientRect();
        const margin = 8;
        const width = Math.max(Math.ceil(r.width), 110);
        menu.style.left = Math.max(margin, Math.min(r.left, window.innerWidth - width - margin)) + 'px';
        menu.style.bottom = (window.innerHeight - r.top + 4) + 'px';
        menu.style.minWidth = width + 'px';
        menu.classList.add('visible');
        const act = menu.querySelector('.command-choice-option.active');
        if (act) act.scrollIntoView({ block: 'nearest' });
    }

    function closeChoiceMenu() {
        choiceOpenFor = null;
        if (choiceMenu) choiceMenu.classList.remove('visible');
    }

    function setChoiceValue(el, v) {
        el.value = v;
        el.querySelector('span').textContent = v || el.dataset.placeholder;
        el.classList.toggle('placeholder', !v);
        const pill = el.closest('.command-part');
        if (pill) pill.classList.remove('invalid');
    }

    function pickChoice(v) {
        if (!choiceOpenFor) return;
        const { el, idx } = choiceOpenFor;
        if (el.tagName === 'BUTTON') {
            setChoiceValue(el, v);
        } else {
            // A User param: the field carries the canonical npub.
            el.value = v;
            const pill = el.closest('.command-part');
            if (pill) pill.classList.remove('invalid');
            const part = composing && composing.parts[idx];
            if (part) part.autoSize();
        }
        closeChoiceMenu();
        // Chosen = filled: flow onward (stay put on the last part).
        const parts = composing ? composing.parts : [];
        if (idx < parts.length - 1) focusPart(idx + 1, 'start');
        else el.focus();
    }

    /** The User-param member menu: the @mention pool filtered by the typed
     *  query (name or npub), avatar rows, capped at 6. */
    function openUserMenu(el, idx) {
        const q = (el.value || '').toLowerCase();
        const options = (io.mentionCandidates ? io.mentionCandidates() : [])
            .filter(c => c.name.toLowerCase().includes(q) || c.npub.toLowerCase().includes(q))
            .slice(0, 6)
            .map(c => ({ v: c.npub, label: c.name, avatarSrc: c.avatarSrc || null }));
        if (!options.length) {
            if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
            return;
        }
        choiceOpenFor = { el, idx, options, active: 0 };
        renderChoiceMenu();
    }

    /** User-input keys: menu navigation when open, part-walking otherwise
     *  (typing itself flows to the input and re-filters via its input event). */
    function onUserKey(e, idx) {
        const el = e.currentTarget;
        if (choiceOpenFor && choiceOpenFor.el === el) {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                e.preventDefault();
                e.stopPropagation();
                const n = choiceOpenFor.options.length;
                choiceOpenFor.active = (choiceOpenFor.active + (e.key === 'ArrowDown' ? 1 : -1) + n) % n;
                renderChoiceMenu();
            } else if (e.key === 'Enter') {
                e.preventDefault();
                e.stopPropagation();
                pickChoice(choiceOpenFor.options[choiceOpenFor.active].v);
            } else if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                closeChoiceMenu();
            } else if (e.key === 'Tab') {
                closeChoiceMenu();
            } else {
                onPartKey(e, idx);
            }
            return;
        }
        onPartKey(e, idx);
    }

    /** Trigger keys: menu-open handling first, open shortcuts second,
     *  everything else falls through to the shared part-walking. */
    function onChoiceKey(e, idx, arg) {
        const el = e.currentTarget;
        if (choiceOpenFor && choiceOpenFor.el === el) {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                e.preventDefault();
                e.stopPropagation();
                const n = choiceOpenFor.options.length;
                choiceOpenFor.active = (choiceOpenFor.active + (e.key === 'ArrowDown' ? 1 : -1) + n) % n;
                renderChoiceMenu();
            } else if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                e.stopPropagation();
                pickChoice(choiceOpenFor.options[choiceOpenFor.active].v);
            } else if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                closeChoiceMenu();
            } else if (e.key === 'Tab') {
                closeChoiceMenu();
            }
            return;
        }
        if (e.key === ' ' || e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            e.stopPropagation();
            openChoiceMenu(el, idx, arg);
            return;
        }
        onPartKey(e, idx);
    }

    /** Focus a part with the caret placed at one end of its value. */
    function focusPart(idx, where) {
        const el = composing && composing.parts[idx] ? composing.parts[idx].el : null;
        if (!el) return;
        el.focus();
        if (el.setSelectionRange) {
            const pos = where === 'end' ? el.value.length : 0;
            el.setSelectionRange(pos, pos);
        }
    }

    /** The JS twin of the Rust `command_text` builder: values with spaces or
     *  quotes are quoted with `\"` escapes, so the assembled text re-parses to
     *  exactly these arguments on the bot side. */
    function assembleCommandText(name, values) {
        let out = '/' + name;
        for (const v of values) {
            out += ' ';
            if (v === '' || /[\s"]/.test(v)) {
                out += '"' + v.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
            } else {
                out += v;
            }
        }
        return out;
    }

    function submitComposer() {
        if (!composing) return;
        if (io.chatId() !== composing.chatId) {
            exitComposer();
            return;
        }
        const { cmd, parts } = composing;
        const values = parts.map((p) => (p.el.value || '').trim());
        let lastFilled = -1;
        values.forEach((v, i) => {
            if (v !== '') lastFilled = i;
        });
        const markInvalid = (i) => {
            parts[i].el.closest('.command-part').classList.add('invalid');
            parts[i].el.focus();
        };
        // Positional wire format: every required part present, no holes before
        // the last provided value, every provided value well-typed.
        for (let i = 0; i < parts.length; i++) {
            const empty = values[i] === '';
            if (empty && (parts[i].arg.required || i < lastFilled)) return markInvalid(i);
            if (!empty && argTypeError(parts[i].arg, values[i])) return markInvalid(i);
        }
        const text = assembleCommandText(cmd.name, values.slice(0, lastFilled + 1));
        armedPick = { chatId: composing.chatId, bot: cmd.bot, name: cmd.name };
        exitComposer(true); // keep the pick: routeForSend resolves the bot tag from it
        io.submit(text);
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
        isComposing,
        submitComposer,
        exitComposer() { exitComposer(false); },
        routeForSend,
        onCommandsUpdated,
        destroy() {
            exitComposer(false);
            textarea.removeEventListener('input', onInput);
            textarea.removeEventListener('keydown', onKeyDown);
            textarea.removeEventListener('blur', onBlur);
            if (panel.parentNode) panel.parentNode.removeChild(panel);
            if (choiceMenu && choiceMenu.parentNode) choiceMenu.parentNode.removeChild(choiceMenu);
        }
    };
}
