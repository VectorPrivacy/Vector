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
 *   const ctrl = initCommandSelector(textarea, io);
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
function initCommandSelector(textarea, io) {
    const RECENTS_CAP = 8;

    let mode = 'closed';        // closed | loading | list | hint
    let activeIndex = 0;        // keyboard-highlighted row (list mode)
    let query = '';             // text typed after '/'
    let open = false;
    let flatRows = [];          // keyboard order = render order (recents, then sections)
    let armedPick = null;       // {chatId, bot, name} — the explicitly chosen row
    let hintSuppressedFor = null; // draft value the user Esc'd the hint away for
    let blurTimer = null;       // pending blur-hide; cancelled if input re-engages
    const snapshots = new Map(); // chatId → {bots, commands}
    const loading = new Set();   // chatIds with a load() in flight

    function isVisible() { return open; }
    function show(view) {
        open = true;
        VectorSvelte.openPopup('command', view);
    }
    function hide() {
        if (open) {
            open = false;
            VectorSvelte.closePopup('command');
        }
        mode = 'closed';
        activeIndex = 0;
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
            // The timeline may have painted before this resolved; let it upgrade
            // any untagged `/cmd args` rows now that the command set is known.
            if (io.commandsReady) io.commandsReady(chatId);
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
        if (io.commandsReady) io.commandsReady(chatId);
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
            // `""` is the positional hole marker (mirrors the Rust parser):
            // an explicitly empty token skips an optional arg entirely.
            if (value === '' && !a.required) continue;
            args.push({ name: a.name, value });
        }
        return args;
    }

    /** One value against one arg spec. Returns an error suffix or null. */
    function argTypeError(a, v) {
        // Wire cap: the manifest parser drops any longer value, which would
        // silently demote the whole invocation to ordinary chat on the bot
        // side. The cap is BYTES (multibyte text exceeds it before .length).
        if (new TextEncoder().encode(v).length > 1024) return 'is too long (max 1024 characters)';
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
        // Bare names: required carries full opacity (+ the composer's
        // asterisk), optional dims via the .optional class — brackets only
        // spent space.
        return a.name;
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

    function commandRow(cmd, flatIndex, showBot) {
        // Recents mix bots, so each row wears its owner's face — two bots'
        // /roll entries are distinct commands that would otherwise look
        // like duplicates.
        const profile = showBot ? (io.botProfile(cmd.bot) || {}) : null;
        return {
            key: cmd.bot + ':' + cmd.name,
            index: flatIndex,
            name: cmd.name,
            args: cmd.args.map(a => ({ label: argSignature(a), optional: !a.required })),
            description: cmd.description || '',
            bot: profile ? { name: profile.name || cmd.bot.slice(0, 12) + '…', avatarSrc: profile.avatarSrc || null } : null,
        };
    }

    function render() {
        const chatId = io.chatId();
        const snap = snapshots.get(chatId);

        // Still fetching and nothing known: the loading state ("Loading N bots").
        if (!snap || (loading.has(chatId) && !allCommands().length)) {
            const n = snap ? snap.bots : 0;
            if (snap && n === 0) { hide(); return; } // known: no bots here
            mode = 'loading';
            show({
                mode: 'loading',
                label: n > 0 ? ('Loading ' + n + ' bot' + (n === 1 ? '' : 's') + '…') : 'Looking for bots…',
            });
            return;
        }

        if (snap.bots === 0) { hide(); return; } // no bots here — nothing to offer
        if (!allCommands().length) {
            // Bots ARE present but none publish a command manifest. Say so
            // rather than silently hiding a deliberately-opened picker; if a
            // manifest is still converging, show that instead of a false empty.
            mode = 'list';
            flatRows = [];
            activeIndex = 0;
            show(snap.fresh === false
                ? { mode: 'message', variant: 'refreshing', label: 'Checking for commands…' }
                : { mode: 'message', variant: 'empty', label: 'No commands available' });
            return;
        }

        const { recent, matches } = visibleRows();
        if (!matches.length) { hide(); return; }
        mode = 'list';

        // Flat keyboard order = exactly the render order (recents, then sections).
        // A stale-served list is still converging (manifest REQ in flight): each
        // bot header carries the status inline so a bot that pops in later isn't
        // a surprise.
        const flat = [];
        const sections = [];
        const refreshing = snap.fresh === false;
        const section = (key, title, avatarSrc, cmds, showBot, refresh) => {
            const rows = [];
            for (const cmd of cmds) {
                rows.push(commandRow(cmd, flat.length, showBot));
                flat.push(cmd);
            }
            sections.push({ key, title, avatarSrc, refreshing: refresh, rows });
        };
        if (recent.length) {
            section('recent', 'Recently Used', null, recent, true, false);
        }
        const byBot = new Map();
        for (const cmd of matches) {
            if (!byBot.has(cmd.bot)) byBot.set(cmd.bot, []);
            byBot.get(cmd.bot).push(cmd);
        }
        for (const [bot, cmds] of byBot) {
            const profile = io.botProfile(bot) || {};
            section(bot, profile.name || (bot.slice(0, 12) + '…'), profile.avatarSrc || null, cmds, false, refreshing);
        }
        flatRows = flat;
        if (activeIndex >= flat.length) activeIndex = 0;
        show({ mode: 'list', sections, active: activeIndex, pick: (i) => selectCommand(flat[i]) });
    }

    /** The armed-command hint bar: signature with the CURRENT arg highlighted;
     *  a Choice arg additionally offers its values as clickable chips. */
    function renderHint(cmd, typedRest) {
        mode = 'hint';

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
        const current = cmd.args[currentIdx];
        show({
            mode: 'hint',
            name: cmd.name,
            args: cmd.args.map((a, i) => ({
                label: argSignature(a),
                optional: !a.required,
                current: i === currentIdx && cmd.args.length > 0,
                title: a.description || '',
            })),
            desc: (current && current.description) || '',
            choices: current && current.type === 'choice' ? (current.choices || []) : [],
            pickChoice: (v) => insertChoice(v),
        });
    }

    // --- Selection → the structured command composer ---
    // Picking a command with args swaps the textarea for one typed input per
    // argument (quoting/escaping is code's job, never the user's), while the
    // reply-bar-style strip above shows "Using /cmd with Bot". Param-less
    // commands send instantly. Typing manually stays the plain-text path.
    let composing = null; // { cmd, chatId, bar, parts: [{arg, el}] }

    /** The focused param's manifest description, shown in the strip (the
     *  visible twin of the hover tooltip — mobile has no hover). */
    function setContextHint(arg) {
        VectorSvelte.setCommandHint((arg && arg.description) || '');
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
        closeChoiceMenu();
        if (!composing) return;
        // Keep the Android back stack in sync when we close via our own paths
        // (Esc, cancel, send, chat switch); no-op after a hardware back pop.
        popBack('command-composer');
        composing = null;
        VectorSvelte.clearCommand();
        VectorSvelte.flushSync();   // the editor is back before it takes focus
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

        textarea.value = '';
        const parts = cmd.args.map(a => ({ arg: a, el: null, autoSize: () => {} }));
        composing = { cmd, chatId: io.chatId(), parts };
        const profile = io.botProfile(cmd.bot) || {};
        VectorSvelte.setCommand({
            name: cmd.name,
            bot: { name: profile.name || cmd.bot.slice(0, 12) + '…', avatarSrc: profile.avatarSrc || null },
            args: cmd.args.map((a, i) => ({
                name: a.name,
                type: a.type,
                required: !!a.required,
                description: a.description || '',
                // The trailing free-text arg (the greedy tail on the wire) takes the row's rest.
                grow: a.type === 'string' && i === cmd.args.length - 1,
            })),
            attach: attachPart,
        });
        // Mount the pills now: the fields need layout to size, and the first needs focus.
        VectorSvelte.flushSync();
        for (const p of parts) p.autoSize();
        io.composerToggled(true);
        // Android hardware back closes the composer first, like Esc on desktop.
        pushBack('command-composer', () => exitComposer(false));
        focusPart(0);
    }

    /** A mounted field: wire its behaviour and register it as `idx`'s part. */
    function attachPart(el, idx) {
        if (!composing || !composing.parts[idx]) return null;
        const part = composing.parts[idx];
        const a = part.arg;
        const wrap = el.closest('.command-part');
        const grows = a.type === 'string' && idx === composing.parts.length - 1;
        if (a.type === 'choice' || a.type === 'bool') {
            el.addEventListener('mousedown', (e) => {
                e.preventDefault();
                el.focus();
                if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
                else openChoiceMenu(el, idx, a);
            });
            // Instant: menu rows preventDefault their mousedown (focus never
            // leaves for a pick), so blur only means a REAL focus move.
            el.addEventListener('blur', () => {
                if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
            });
        } else if (a.type === 'int' || a.type === 'number') {
            // inputMode only picks the MOBILE keypad — desktops can type
            // anything, so filter illegal characters live (digits, one leading
            // minus, one dot for Number).
            el.addEventListener('input', () => {
                const caret = el.selectionStart;
                let v = el.value.replace(a.type === 'int' ? /[^\d-]/g : /[^\d.\-]/g, '')
                    .replace(/(?!^)-/g, '');
                const dot = v.indexOf('.');
                if (dot !== -1) v = v.slice(0, dot + 1) + v.slice(dot + 1).replace(/\./g, '');
                if (v !== el.value) {
                    const removed = el.value.length - v.length;
                    el.value = v;
                    const pos = Math.max(0, (caret || 0) - removed);
                    el.setSelectionRange(pos, pos);
                }
            });
        }
        // Width = MEASURED text pixels (ch-guessing undershoots on wide glyphs
        // like m/w and wraps too early) + padding + caret slack; border-box, so
        // the CSS max-width still does the wide-then-wrap clamp. field-sizing
        // isn't in WKWebView.
        const autoSize = () => {
            if (el.tagName === 'TEXTAREA') {
                if (!grows) el.style.width = Math.max(72, Math.ceil(measureFieldText(el)) + 26) + 'px';
                el.style.height = 'auto';
                el.style.height = el.scrollHeight + 'px';
                if (wrap) wrap.classList.toggle('multiline', el.offsetHeight > 30);
            } else if (el.tagName === 'INPUT') {
                el.style.width = Math.max(72, Math.ceil(measureFieldText(el)) + 26) + 'px';
            }
        };
        if (a.type === 'choice' || a.type === 'bool') {
            el.addEventListener('keydown', (e) => onChoiceKey(e, idx, a));
        } else if (a.type === 'user') {
            el.addEventListener('keydown', (e) => onUserKey(e, idx));
            el.addEventListener('focus', () => openUserMenu(el, idx));
            el.addEventListener('input', () => {
                // Typing dissolves a picked member back to raw text.
                delete el.dataset.npub;
                el.classList.remove('user-resolved');
                openUserMenu(el, idx);
            });
            // Instant: row picks preventDefault their mousedown, so blur
            // only means a REAL focus move (arrow-walking included).
            el.addEventListener('blur', () => {
                if (choiceOpenFor && choiceOpenFor.el === el) closeChoiceMenu();
            });
        } else {
            el.addEventListener('keydown', (e) => onPartKey(e, idx));
        }
        el.addEventListener('focus', () => setContextHint(a));
        el.addEventListener('input', () => {
            VectorSvelte.setCommandInvalid(-1);
            autoSize();
        });
        el.addEventListener('change', () => VectorSvelte.setCommandInvalid(-1));
        part.el = el;
        part.autoSize = autoSize;
        return null;
    }

    // Advancing INTO a picker (choice/bool/user) via Enter auto-opens its menu,
    // and the SAME keypress trails onto the newly focused trigger — which would
    // instantly pick the highlighted option and skip the picker entirely. Guard
    // from the advance until the key releases so that trailing edge is swallowed;
    // a fresh press still picks. keyup clears it on desktop; the timeout is the
    // fallback for soft keyboards that don't emit keyup (touch picks a row via
    // pointer, so they're unaffected either way).
    let advanceKeyGuard = false;
    function guardAdvanceKey() {
        advanceKeyGuard = true;
        window.addEventListener('keyup', () => { advanceKeyGuard = false; }, { once: true });
        setTimeout(() => { advanceKeyGuard = false; }, 400);
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
            else { guardAdvanceKey(); focusPart(idx + 1); }
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

    // ── The Choice drop-up (one menu, anchored to the focused trigger) ──────
    let choiceOpenFor = null; // { el: trigger, idx, options: [{v, label}], active }

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
        VectorSvelte.openChoiceMenu({ anchor: el, options, active, pick: pickChoice });
    }

    function closeChoiceMenu() {
        choiceOpenFor = null;
        VectorSvelte.closeChoiceMenu();
    }

    function setChoiceValue(el, v) {
        el.value = v;
        const idx = composing ? composing.parts.findIndex(p => p.el === el) : -1;
        VectorSvelte.setCommandValue(idx, v);
        VectorSvelte.setCommandInvalid(-1);
    }

    function pickChoice(opt) {
        if (!choiceOpenFor) return;
        const { el, idx } = choiceOpenFor;
        if (el.tagName === 'BUTTON') {
            setChoiceValue(el, opt.v);
        } else {
            // A User param DISPLAYS the member's name; the canonical npub
            // rides in data-npub and submit prefers it. Any later typing
            // dissolves the resolution back to raw text.
            el.value = opt.label;
            el.dataset.npub = opt.v;
            el.classList.add('user-resolved');
            VectorSvelte.setCommandInvalid(-1);
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
            // Same placeholder fallback the @mention selector uses, so
            // avatarless members still get a face in the row.
            .map(c => ({ v: c.npub, label: c.name, avatarSrc: c.avatarSrc || 'icons/user-placeholder.svg' }));
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
        if (advanceKeyGuard && e.key === 'Enter') {
            e.preventDefault();
            e.stopPropagation();
            return;
        }
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
                guardAdvanceKey();
                pickChoice(choiceOpenFor.options[choiceOpenFor.active]);
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
        // Swallow the trailing edge of the Enter/Space that advanced us onto this
        // trigger, so the auto-opened menu stays put instead of instant-picking.
        if (advanceKeyGuard && (e.key === 'Enter' || e.key === ' ')) {
            e.preventDefault();
            e.stopPropagation();
            return;
        }
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
                guardAdvanceKey();
                pickChoice(choiceOpenFor.options[choiceOpenFor.active]);
            } else if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                closeChoiceMenu();
            } else if (e.key === 'Tab') {
                closeChoiceMenu();
            } else if (e.key === 'Backspace' || e.key === 'Delete') {
                // Same walk the text parts have: a picked value clears first
                // (the menu stays up for a re-pick), an empty trigger walks
                // backwards — and past the first part cancels the command.
                e.preventDefault();
                e.stopPropagation();
                if (el.value) { setChoiceValue(el, ''); renderChoiceMenu(); return; }
                closeChoiceMenu();
                if (idx === 0) { exitComposer(); return; }
                focusPart(idx - 1, 'end');
            }
            return;
        }
        if (e.key === ' ' || e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            e.stopPropagation();
            openChoiceMenu(el, idx, arg);
            return;
        }
        if ((e.key === 'Backspace' || e.key === 'Delete') && el.value) {
            // Closed-menu twin of the branch above; the empty case falls
            // through to onPartKey's ordinary backwards walk.
            e.preventDefault();
            setChoiceValue(el, '');
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
        // Landing on a Choice/Bool trigger opens its drop-up, so advancing between
        // args flows straight into the picker (User params open on their own focus).
        const arg = composing.parts[idx].arg;
        if (arg.type === 'choice' || arg.type === 'bool') openChoiceMenu(el, idx, arg);
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
        // The wire prefers a picked member's canonical npub over the displayed name.
        const values = parts.map((p) =>
            (p.arg.type === 'user' && p.el.dataset.npub ? p.el.dataset.npub : p.el.value || '').trim()
        );
        let lastFilled = -1;
        values.forEach((v, i) => {
            if (v !== '') lastFilled = i;
        });
        const markInvalid = (i) => {
            VectorSvelte.setCommandInvalid(i);
            parts[i].el.focus();
        };
        // Positional wire format: every required part present, every provided
        // value well-typed. A skipped optional before a filled arg is fine —
        // it rides the wire as the `""` hole marker.
        for (let i = 0; i < parts.length; i++) {
            const empty = values[i] === '';
            if (empty && parts[i].arg.required) return markInvalid(i);
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
        // Any input means focus is present, so a pending blur-hide is stale —
        // cancel it (a programmatic prefill re-opens right after a blur, and the
        // 150ms timer would otherwise flash the panel shut).
        if (blurTimer) { clearTimeout(blurTimer); blurTimer = null; }
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
        const flat = flatRows;
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
        blurTimer = setTimeout(() => { blurTimer = null; if (isVisible()) hide(); }, 150);
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
        /** Whether `chatId` (default: the open chat) has any known bots. Also
         *  warms the snapshot, so a later caller (e.g. the attachment menu) sees
         *  it even on the first look at a chat. */
        hasBots(chatId) {
            const id = chatId || io.chatId();
            if (!id) return false;
            ensureLoaded(id, false);
            const snap = snapshots.get(id);
            return !!(snap && snap.bots > 0);
        },
        /** The declared command names for a chat, so the timeline can recognise
         *  an untagged `/name args` invocation (a 1:1 DM sends them untagged).
         *  Null when the snapshot is cold — the caller then promotes nothing. */
        commandNames(chatId) {
            const snap = snapshots.get(chatId || io.chatId());
            if (!snap || !snap.commands) return null;
            const names = new Set();
            for (const b of snap.commands) {
                for (const c of b.commands || []) names.add(c.name);
            }
            return names;
        },
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
            hide();
        }
    };
}
