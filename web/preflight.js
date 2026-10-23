'use strict';
// Vector Web's first script: can this browser run Vector at all, and if not, why and what to do.
// Written for the oldest browsers on purpose (no arrows, let, classes or template strings), so it
// runs and explains where every later script would only throw. It tests what Vector uses, not
// version numbers, and draws every full-screen notice the page shows (`__vectorNotice`).
(function () {
    var ua = navigator.userAgent || '';
    var num = function (m, i) { return m ? parseInt(m[i || 1], 10) : 0; };
    var ios = /\b(iPhone|iPad|iPod)\b/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
    var iosMatch = /OS (\d+)_(\d+)/.exec(ua) || (ios ? /Version\/(\d+)\.(\d+)/.exec(ua) : null);
    var firefox = /Firefox\/(\d+)/.exec(ua);
    var chromium = /Chrome\/(\d+)/.exec(ua);
    var safari = !ios && !firefox && !chromium && /Safari\//.test(ua) ? /Version\/(\d+)\.(\d+)/.exec(ua) : null;
    var env = {
        ios: ios,
        iosVersion: iosMatch ? num(iosMatch) + '.' + num(iosMatch, 2) : '',
        iosMajor: num(iosMatch),
        android: /Android/.test(ua),
        firefox: !!firefox,
        chromium: !!chromium,
        edge: /Edg\//.test(ua),
        safari: !!safari,
        safariMajor: num(safari),
    };

    // Tiny modules for the WebAssembly features Rust's default target uses.
    var WASM_FEATURES = {
        'bulk memory': [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 5, 3, 1, 0, 1, 10, 14, 1, 12, 0, 65, 0, 65, 0, 65, 0, 252, 10, 0, 0, 11],
        'reference types': [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 7, 1, 5, 0, 208, 112, 26, 11],
        'float to int': [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 12, 1, 10, 0, 67, 0, 0, 0, 0, 252, 0, 26, 11],
        'sign extension': [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 8, 1, 6, 0, 65, 0, 192, 26, 11],
        'multi value': [0, 97, 115, 109, 1, 0, 0, 0, 1, 6, 1, 96, 0, 2, 127, 127, 3, 2, 1, 0, 10, 8, 1, 6, 0, 65, 0, 65, 0, 11],
    };

    // What the page's own scripts need: Safari 15.4, Firefox 125, Chrome 98.
    function missingFeatures() {
        var out = [];
        var has = function (ok, name) { if (!ok) out.push(name); };
        has(typeof Promise === 'function' && typeof Proxy === 'function', 'Promise');
        has(typeof Worker === 'function', 'Worker');
        has(typeof BroadcastChannel === 'function', 'BroadcastChannel');
        has(typeof structuredClone === 'function', 'structuredClone');
        has(typeof Object.hasOwn === 'function', 'Object.hasOwn');
        has(typeof BigInt === 'function', 'BigInt');
        has(typeof Intl === 'object' && typeof Intl.Segmenter === 'function', 'Intl.Segmenter');
        has(typeof crypto === 'object' && typeof crypto.randomUUID === 'function', 'crypto.randomUUID');
        if (typeof WebAssembly === 'object' && typeof WebAssembly.validate === 'function') {
            for (var name in WASM_FEATURES) {
                var ok = false;
                try { ok = WebAssembly.validate(new Uint8Array(WASM_FEATURES[name])); } catch (e) { ok = false; }
                has(ok, 'WebAssembly ' + name);
            }
        }
        return out;
    }

    // ── what to say ──

    function tooOld(detail) {
        if (env.ios) {
            return {
                title: 'Update iOS to use Vector',
                // Newer Safari freezes the version it reports, so only an older one is quoted.
                body: 'Vector Web needs iOS 15.4 or newer' + (env.iosMajor && env.iosMajor < 16 ? ', and this device runs iOS ' + env.iosVersion + '.' : '.'),
                steps: ['Settings > General > Software Update'],
                detail: detail,
            };
        }
        if (env.safari) {
            return {
                title: 'Update Safari to use Vector',
                body: 'Vector Web needs Safari 15.4 or newer.',
                steps: ['Apple menu > System Settings > General > Software Update'],
                detail: detail,
            };
        }
        if (env.firefox) {
            return {
                title: 'Update Firefox to use Vector',
                body: 'Vector Web needs Firefox 125 or newer, or the latest Tor Browser.',
                steps: [env.android ? 'Update Firefox from your app store.' : 'Menu > Help > About Firefox'],
                detail: detail,
            };
        }
        if (env.chromium) {
            return {
                title: 'Update your browser to use Vector',
                body: 'Vector Web needs Chrome 98 or newer, or a browser built on it.',
                steps: [env.android ? 'Update your browser from your app store.' : 'Menu > Help > About'],
                detail: detail,
            };
        }
        return {
            title: 'Update your browser to use Vector',
            body: 'Vector Web runs in current versions of Chrome, Firefox and Safari.',
            detail: detail,
        };
    }

    var RELOAD = { label: 'Reload', run: function () { location.reload(); } };

    // WebAssembly switched off in a browser new enough to have it.
    function wasmOff() {
        var site = location.host;
        if (env.ios && env.iosMajor >= 16) {
            return {
                title: 'Turn off Lockdown Mode for Vector',
                body: 'Lockdown Mode switches off WebAssembly, which Vector runs on. Safari can exclude just this site.',
                steps: ['Safari: page menu in the address bar > Website Settings', 'Turn off Lockdown Mode, then reload this page.'],
                action: RELOAD,
            };
        }
        if (env.safari && env.safariMajor >= 16) {
            return {
                title: 'Turn off Lockdown Mode for Vector',
                body: 'Lockdown Mode switches off WebAssembly, which Vector runs on. Safari can exclude just this site.',
                steps: ['Safari menu > Settings for ' + site, 'Uncheck Enable Lockdown Mode, then reload this page.'],
                action: RELOAD,
            };
        }
        if (env.firefox) {
            return {
                title: 'Turn on WebAssembly for Vector',
                body: 'This browser has WebAssembly switched off, and Vector runs on it.',
                steps: [
                    'Tor Browser or Mullvad Browser: shield menu > Security Level > Standard',
                    'Firefox: about:config > javascript.options.wasm > true',
                    'Reload this page.',
                ],
                action: RELOAD,
            };
        }
        if (env.chromium && env.android) {
            return {
                title: 'Allow JavaScript JIT for Vector',
                body: 'This browser runs the site with its JavaScript optimizer off, which also switches off WebAssembly.',
                steps: [
                    'Icon left of the address > Permissions (or Site settings)',
                    'Allow JavaScript JIT (in Chrome, the V8 optimizer), then reload this page.',
                ],
                action: RELOAD,
            };
        }
        if (env.chromium) {
            return {
                title: 'Turn on WebAssembly for Vector',
                body: 'This browser runs the site with its JavaScript optimizer off, which also switches off WebAssembly.',
                steps: [
                    env.edge
                        ? 'Edge: Settings > Privacy > Enhance your security on the web > Exceptions > add ' + site
                        : 'Chrome: Settings > Privacy and security > Site settings > V8 optimizer > allow ' + site,
                    'Reload this page.',
                ],
                action: RELOAD,
            };
        }
        return {
            title: 'Turn on WebAssembly for Vector',
            body: 'This browser has WebAssembly switched off, and Vector runs on it. Allow it for this site in the browser\'s settings.',
            action: RELOAD,
        };
    }

    // Why the engine failed to start, from what the worker reported.
    function boot(error, offline) {
        var text = String(error || '');
        if (offline || /Failed to fetch|NetworkError|Load failed|network error/i.test(text)) {
            return {
                title: 'Vector couldn\'t download',
                body: 'Check your connection, then reload.',
                detail: text,
                action: RELOAD,
            };
        }
        if (/QuotaExceeded|quota/i.test(text)) {
            return {
                title: 'Vector needs more space',
                body: 'This device is out of storage for Vector. Free some up, then reload.',
                detail: text,
                action: RELOAD,
            };
        }
        if (/CompileError|LinkError|WebAssembly\.(Module|instantiate|compile)|invalid opcode|unknown opcode|Unsupported|SyntaxError|Unexpected/i.test(text)) {
            return tooOld(text);
        }
        return {
            title: 'Vector couldn\'t start',
            body: 'Something stopped it while loading. Reloading usually sorts it out.',
            detail: text,
            action: RELOAD,
        };
    }

    // ── drawing it ──

    var current = null;

    function el(tag, css, text) {
        var node = document.createElement(tag);
        if (css) node.style.cssText = css;
        if (text !== undefined) node.textContent = text;
        return node;
    }

    var FONT = 'font-family:Rubik,system-ui,-apple-system,"Segoe UI",sans-serif;';

    // One notice at a time, over everything: the popup's icon, title and line, any steps as a
    // numbered list, the error itself small and grey, and a button only where it helps.
    function notice(o) {
        var show = function () {
            if (current && current.parentNode) current.parentNode.removeChild(current);
            var root = el('div', 'position:fixed;top:0;right:0;bottom:0;left:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;padding:28px 22px;box-sizing:border-box;overflow-y:auto;background:#030303;text-align:center;' + FONT);
            root.setAttribute('role', 'alert');
            var card = el('div', 'max-width:400px;width:100%;display:flex;flex-direction:column;align-items:center');
            var icon = el('img', 'width:76px;height:76px;margin-bottom:14px');
            icon.src = '/icons/vector_warning.svg';
            icon.alt = '';
            card.appendChild(icon);
            card.appendChild(el('h2', 'margin:0 0 8px;color:#fff;font-size:21px;line-height:27px;font-weight:600', o.title));
            if (o.body) card.appendChild(el('p', 'margin:0;color:#9a9a9a;font-size:15px;line-height:1.5', o.body));
            if (o.steps && o.steps.length) {
                var list = el('ol', 'margin:18px 0 0;padding:14px 16px 14px 34px;width:100%;box-sizing:border-box;border-radius:10px;background:#111;text-align:left;color:#e6e6e6;font-size:14px;line-height:1.5');
                for (var i = 0; i < o.steps.length; i++) list.appendChild(el('li', i ? 'margin-top:8px' : '', o.steps[i]));
                card.appendChild(list);
            }
            if (o.detail) card.appendChild(el('p', 'margin:14px 0 0;color:#5f5f5f;font-size:12px;line-height:1.45;word-break:break-word', String(o.detail).slice(0, 300)));
            if (o.action) {
                var button = el('button', 'margin-top:22px;padding:11px 26px;border:0;border-radius:8px;background:#59fcb3;color:#030303;font-size:15px;font-weight:600;cursor:pointer;' + FONT, o.action.label);
                button.type = 'button';
                button.onclick = o.action.run;
                card.appendChild(button);
            }
            root.appendChild(card);
            document.body.appendChild(root);
            current = root;
        };
        if (document.body) show(); else document.addEventListener('DOMContentLoaded', show);
    }

    // A quiet card at the foot of the screen that leaves the page usable.
    var hintEl = null;
    function hint(text) {
        var show = function () {
            if (hintEl) return;
            hintEl = el('div', 'position:fixed;left:16px;right:16px;bottom:24px;z-index:2147483646;margin:0 auto;max-width:420px;padding:12px 14px;border-radius:10px;background:#161616;box-shadow:0 8px 28px rgba(0,0,0,.5);color:#cfcfcf;font-size:13px;line-height:1.45;text-align:left;' + FONT, text);
            document.body.appendChild(hintEl);
        };
        if (document.body) show(); else document.addEventListener('DOMContentLoaded', show);
    }
    function unhint() {
        if (hintEl && hintEl.parentNode) hintEl.parentNode.removeChild(hintEl);
        hintEl = null;
    }

    window.__vectorNotice = notice;
    window.__vectorHint = hint;
    window.__vectorUnhint = unhint;
    window.__vectorDiagnoseBoot = function (error) { return boot(error, navigator.onLine === false); };
    window.__vectorEnv = env;

    // ── the check ──

    var missing = missingFeatures();
    var noWasm = typeof WebAssembly !== 'object';
    var block = null;
    if (missing.length) block = tooOld('Missing: ' + missing.join(', '));
    else if (noWasm) block = wasmOff();
    if (block) {
        window.__vectorBlocked = block;
        notice(block);
    }
})();
