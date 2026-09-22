#!/usr/bin/env node
/**
 * check-globals.mjs — the one-global-scope frontend's missing-name check.
 *
 * The plain scripts in index.html share one scope, so a global one file deletes is a
 * ReferenceError in another file at the moment that path runs, not at load. This parses
 * every first-party script, collects every name any of them declares (any depth), and
 * reports identifiers that none declares and the browser does not provide. It also checks
 * that every `VectorSvelte.<name>` the scripts use is exported by src/components/index.js,
 * that nothing on that surface is dead, and that no export shares a name with a global (a
 * bare call would then reach the vanilla one and the prefixed call the bundle's). Last, it
 * checks the string-keyed shell registries: a `setScreen('x')` naming a screen the shell never
 * declared throws at load and the component simply never mounts, with nothing on screen to
 * say why.
 *
 * Usage: node scripts/check-globals.mjs   (exit 1 on findings)
 */
import { readFileSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import * as acorn from 'acorn';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'src');

// The load order is index.html's; vendored and minified libraries are consumers' globals.
const html = readFileSync(join(SRC, 'index.html'), 'utf8');
const VENDORED = /(\.min\.js|jsqr\.js|qrcode-generator\.js|twemoji|components\.bundle\.js)$/;
const scriptTags = [...html.matchAll(/<script src="\/?([^"]+)"([^>]*)>/g)].filter(m => !VENDORED.test(m[1]));
const scripts = scriptTags.map(m => m[1]);
// Execution order: a plain script runs while the document parses, every deferred one after,
// each group in document order. A top-level call that names a helper by value reaches a
// script that has not run yet as a ReferenceError, and the call registers nothing.
const runOrder = new Map();
[...scriptTags.filter(m => !/\bdefer\b/.test(m[2])), ...scriptTags.filter(m => /\bdefer\b/.test(m[2]))].forEach((m, i) => runOrder.set(m[1], i));

// What the vendored scripts and the platform give the shared scope.
const PROVIDED = new Set([
    'VectorSvelte', 'qrcode', 'jsQR', 'twemoji', 'DOMPurify', 'hljs', 'marked', '__TAURI__', 'NodeFilter',
    'window', 'document', 'navigator', 'console', 'globalThis', 'self', 'localStorage', 'sessionStorage',
    'setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'requestAnimationFrame', 'cancelAnimationFrame',
    'requestIdleCallback', 'cancelIdleCallback', 'queueMicrotask', 'structuredClone', 'fetch', 'Request', 'Response', 'Headers',
    'AbortController', 'AbortSignal', 'URL', 'URLSearchParams', 'Blob', 'File', 'FileReader', 'FormData', 'TextEncoder', 'TextDecoder',
    'Image', 'Audio', 'HTMLElement', 'HTMLImageElement', 'HTMLInputElement', 'HTMLTextAreaElement', 'HTMLVideoElement', 'HTMLAudioElement',
    'HTMLCanvasElement', 'HTMLMediaElement', 'Element', 'Node', 'NodeList', 'Event', 'CustomEvent', 'MouseEvent', 'KeyboardEvent', 'PointerEvent',
    'TouchEvent', 'InputEvent', 'ClipboardEvent', 'DragEvent', 'FocusEvent', 'WheelEvent', 'AnimationEvent', 'TransitionEvent', 'MessageEvent',
    'EventTarget', 'MutationObserver', 'IntersectionObserver', 'ResizeObserver', 'PerformanceObserver', 'performance', 'crypto', 'CSS',
    'getComputedStyle', 'matchMedia', 'scrollTo', 'scrollBy', 'alert', 'confirm', 'prompt', 'open', 'close', 'focus', 'blur', 'innerWidth', 'innerHeight',
    'devicePixelRatio', 'screen', 'history', 'location', 'addEventListener', 'removeEventListener', 'dispatchEvent', 'atob', 'btoa',
    'encodeURIComponent', 'decodeURIComponent', 'encodeURI', 'decodeURI', 'escape', 'unescape', 'isNaN', 'isFinite', 'parseInt', 'parseFloat',
    'Math', 'Date', 'JSON', 'Number', 'String', 'Boolean', 'Array', 'Object', 'Function', 'Symbol', 'BigInt', 'RegExp', 'Error', 'TypeError',
    'RangeError', 'SyntaxError', 'ReferenceError', 'DOMException', 'Promise', 'Map', 'Set', 'WeakMap', 'WeakSet', 'WeakRef', 'FinalizationRegistry',
    'Proxy', 'Reflect', 'Intl', 'ArrayBuffer', 'SharedArrayBuffer', 'DataView', 'Uint8Array', 'Uint8ClampedArray', 'Int8Array', 'Uint16Array',
    'Int16Array', 'Uint32Array', 'Int32Array', 'Float32Array', 'Float64Array', 'BigInt64Array', 'BigUint64Array', 'Atomics', 'Infinity', 'NaN',
    'undefined', 'arguments', 'eval', 'AudioContext', 'webkitAudioContext', 'MediaRecorder', 'MediaStream', 'MediaSource', 'AudioBuffer',
    'OfflineAudioContext', 'AnalyserNode', 'GainNode', 'Worker', 'WebSocket', 'VideoEncoder', 'VideoDecoder', 'VideoFrame', 'EncodedVideoChunk', 'XMLHttpRequest', 'DOMParser', 'XMLSerializer', 'Range', 'Selection',
    'getSelection', 'createImageBitmap', 'ImageData', 'ImageBitmap', 'OffscreenCanvas', 'Path2D', 'CanvasRenderingContext2D', 'Notification',
    'speechSynthesis', 'SpeechSynthesisUtterance', 'visualViewport', 'ontouchstart', 'DocumentFragment', 'Text', 'Comment', 'ShadowRoot',
    'HTMLCollection', 'NamedNodeMap', 'CharacterData', 'StaticRange', 'IdleDeadline', 'PromiseRejectionEvent', 'ErrorEvent', 'ProgressEvent',
    'BeforeUnloadEvent', 'PageTransitionEvent', 'HashChangeEvent', 'PopStateEvent', 'StorageEvent', 'CompositionEvent', 'UIEvent',
    'Touch', 'TouchList', 'DataTransfer', 'DataTransferItem', 'DataTransferItemList', 'indexedDB', 'IDBKeyRange', 'caches', 'Cache',
    'ReadableStream', 'WritableStream', 'TransformStream', 'CompressionStream', 'DecompressionStream', 'HTMLDivElement', 'HTMLSpanElement',
    'HTMLButtonElement', 'HTMLAnchorElement', 'HTMLIFrameElement', 'HTMLSelectElement', 'HTMLOptionElement', 'HTMLLabelElement', 'HTMLFormElement',
    'SVGElement', 'SVGSVGElement', 'onerror', 'onunhandledrejection', 'name', 'status', 'length', 'top', 'parent', 'frames', 'origin',
    'PublicKeyCredential', 'AudioWorkletNode', 'MediaStreamAudioSourceNode', 'BiquadFilterNode', 'ScriptProcessorNode', 'AudioBufferSourceNode',
    'Iterator', 'AggregateError', 'WebAssembly', 'process', 'require', 'module', 'exports', 'Buffer',
]);

const declared = new Set();          // every name any first-party script declares, any depth
const declaredIn = new Map();        // name -> the scripts declaring it (any depth)
const topDeclaredIn = new Map();     // name -> the scripts declaring it at top level: what the shared scope actually holds
const perFile = new Map();
const localNames = new Map();        // file → names it declares at any depth           // file → Set of identifiers referenced
const computedUses = [];             // VectorSvelte[expr]: a use the name-based passes cannot see
const eagerCalls = [];               // { file, line, names }: top-level VectorSvelte.* calls and the identifiers their arguments pass by value
const svelteUses = new Map();        // VectorSvelte.<name> → [files]
const svelteUsesIn = new Map();      // file → Set of VectorSvelte.<name> it calls
const registryCalls = [];            // { file, line, fn, name }: a literal key handed to a shell registry

// The shell's registries, each a `$state({ ... })` whose keys are the only names its setters
// accept, and the calls that take one of those keys first.
const REGISTRY_OF = { setScreen: 'screens', showPane: 'panes', paneShown: 'panes', setShellFlag: 'shell', revealPane: 'reveals' };
const registryKeys = new Map();      // registry → Set of declared keys
{
    const shellAst = acorn.parse(readFileSync(join(SRC, 'components/lib/shell.svelte.js'), 'utf8'), { ecmaVersion: 'latest', sourceType: 'module' });
    const wanted = new Set(Object.values(REGISTRY_OF));
    walk(shellAst, (n) => {
        if (n.type !== 'VariableDeclarator' || n.id.type !== 'Identifier' || !wanted.has(n.id.name)) return;
        const obj = n.init?.type === 'CallExpression' && n.init.callee.name === '$state' ? n.init.arguments[0] : null;
        if (obj?.type !== 'ObjectExpression') return;
        registryKeys.set(n.id.name, new Set(obj.properties.map(p => p.key?.name ?? p.key?.value).filter(Boolean)));
    });
}

function collectPattern(node, out) {
    if (!node) return;
    switch (node.type) {
        case 'Identifier': out.add(node.name); break;
        case 'ObjectPattern': for (const p of node.properties) collectPattern(p.type === 'RestElement' ? p.argument : p.value, out); break;
        case 'ArrayPattern': for (const e of node.elements) collectPattern(e, out); break;
        case 'RestElement': collectPattern(node.argument, out); break;
        case 'AssignmentPattern': collectPattern(node.left, out); break;
    }
}

function walk(node, visit) {
    if (!node || typeof node.type !== 'string') return;
    visit(node);
    for (const key of Object.keys(node)) {
        if (key === 'type' || key === 'loc' || key === 'range') continue;
        const v = node[key];
        if (Array.isArray(v)) { for (const c of v) if (c && typeof c.type === 'string') walk(c, visit); }
        else if (v && typeof v.type === 'string') walk(v, visit);
    }
}

for (const rel of scripts) {
    const file = join(SRC, rel);
    const code = readFileSync(file, 'utf8');
    const ast = acorn.parse(code, { ecmaVersion: 'latest', sourceType: 'script', allowHashBang: true, locations: true });
    const refs = new Set();
    const fileDeclared = new Set();
    walk(ast, (n) => {
        switch (n.type) {
            case 'VariableDeclarator': collectPattern(n.id, fileDeclared); break;
            case 'FunctionDeclaration': case 'FunctionExpression': case 'ArrowFunctionExpression':
                if (n.id) fileDeclared.add(n.id.name);
                for (const p of n.params) collectPattern(p, fileDeclared);
                break;
            case 'ClassDeclaration': case 'ClassExpression': if (n.id) fileDeclared.add(n.id.name); break;
            case 'CatchClause': collectPattern(n.param, fileDeclared); break;
            case 'ImportDeclaration': for (const s of n.specifiers) fileDeclared.add(s.local.name); break;
            case 'CallExpression': {
                const c = n.callee;
                const arg = n.arguments[0];
                if (c.type === 'MemberExpression' && c.object.type === 'Identifier' && c.object.name === 'VectorSvelte'
                    && !c.computed && REGISTRY_OF[c.property.name] && arg?.type === 'Literal' && typeof arg.value === 'string') {
                    registryCalls.push({ file: rel, line: n.loc?.start.line, fn: c.property.name, name: arg.value });
                }
                break;
            }
            case 'MemberExpression':
                if (n.object.type === 'Identifier' && n.object.name === 'VectorSvelte' && n.computed) computedUses.push(`${rel}:${n.loc?.start.line}`);
                if (n.object.type === 'Identifier' && n.object.name === 'VectorSvelte' && !n.computed && n.property.type === 'Identifier') {
                    const list = svelteUses.get(n.property.name) || [];
                    list.push(rel);
                    svelteUses.set(n.property.name, list);
                    if (!svelteUsesIn.has(rel)) svelteUsesIn.set(rel, new Set());
                    svelteUsesIn.get(rel).add(n.property.name);
                }
                break;
        }
    });
    // References: identifiers that are not property names, keys or labels.
    walk(ast, (n) => {
        if (n.type === 'MemberExpression' && !n.computed && n.property.type === 'Identifier') n.property.__notRef = true;
        if (n.type === 'Property' && !n.computed && n.key.type === 'Identifier' && !n.shorthand) n.key.__notRef = true;
        if (n.type === 'MethodDefinition' && !n.computed && n.key.type === 'Identifier') n.key.__notRef = true;
        if (n.type === 'PropertyDefinition' && !n.computed && n.key.type === 'Identifier') n.key.__notRef = true;
        if (n.type === 'LabeledStatement') n.label.__notRef = true;
        if ((n.type === 'BreakStatement' || n.type === 'ContinueStatement') && n.label) n.label.__notRef = true;
        if (n.type === 'Identifier' && !n.__notRef) refs.add(n.name);
    });
    perFile.set(rel, refs);
    localNames.set(rel, fileDeclared);
    for (const st of ast.body) {
        const top = new Set();
        if (st.type === 'FunctionDeclaration' || st.type === 'ClassDeclaration') { if (st.id) top.add(st.id.name); }
        if (st.type === 'VariableDeclaration') for (const d of st.declarations) collectPattern(d.id, top);
        for (const name of top) { const l = topDeclaredIn.get(name) || []; l.push(rel); topDeclaredIn.set(name, l); }
        const call = st.type === 'ExpressionStatement' && st.expression.type === 'CallExpression' ? st.expression : null;
        if (!call || call.callee.type !== 'MemberExpression' || call.callee.object.name !== 'VectorSvelte') continue;
        // Only what the argument evaluates now: a reference inside a nested function resolves at call time.
        const eager = new Set();
        (function collect(n, inFn) {
            if (!n || typeof n.type !== 'string') return;
            const fn = inFn || n.type === 'ArrowFunctionExpression' || n.type === 'FunctionExpression';
            if (!fn && n.type === 'Property' && n.value.type === 'Identifier') eager.add(n.value.name);
            for (const k of Object.keys(n)) { const v = n[k]; if (Array.isArray(v)) v.forEach(c => collect(c, fn)); else if (v && typeof v.type === 'string') collect(v, fn); }
        })({ type: 'Arguments', list: call.arguments }, false);
        if (eager.size) eagerCalls.push({ file: rel, line: st.loc?.start.line ?? call.start, names: [...eager] });
    }
    for (const name of fileDeclared) {
        declared.add(name);
        const list = declaredIn.get(name) || [];
        list.push(rel);
        declaredIn.set(name, list);
    }
}

// The bundle's surface: named exports and export lists in components/index.js.
const indexSrc = readFileSync(join(SRC, 'components/index.js'), 'utf8');
const exported = new Set();
for (const m of indexSrc.matchAll(/export\s+(?:async\s+)?function\s+(\w+)/g)) exported.add(m[1]);
for (const m of indexSrc.matchAll(/export\s+(?:const|let)\s+(\w+)/g)) exported.add(m[1]);
for (const m of indexSrc.matchAll(/export\s*\{([^}]+)\}/g)) {
    for (const part of m[1].split(',')) {
        const name = part.trim().split(/\s+as\s+/).pop().trim();
        if (name) exported.add(name);
    }
}

// Exports with no vanilla caller: either a component-only read that does not belong on the
// bundle's surface, or the leftover of a deleted call site. Ones the components legitimately
// share with each other through the bundle are listed here.
const COMPONENT_ONLY = new Set(['flushSync', 'deriveWindow', 'profileEditDirty']);

let findings = 0;
for (const { file, line, names } of eagerCalls) {
    const later = names.filter(n => {
        const owners = topDeclaredIn.get(n);
        if (!owners || owners.includes(file)) return false;
        return owners.every(o => runOrder.get(o) > runOrder.get(file));
    });
    if (later.length) { findings++; console.log(`${file}:${line}: passes ${later.join(', ')} by value at load, but they are declared in a script that runs later`); }
}
for (const [rel, refs] of perFile) {
    const missing = [...refs].filter(n => !topDeclaredIn.has(n) && !PROVIDED.has(n) && !localNames.get(rel)?.has(n)).sort();
    if (missing.length) { findings += missing.length; console.log(`${rel}: undeclared ${missing.join(', ')}`); }
}
for (const [name, files] of svelteUses) {
    if (!exported.has(name)) { findings++; console.log(`VectorSvelte.${name} is not exported (used in ${[...new Set(files)].join(', ')})`); }
}
if (computedUses.length) console.log(`note: computed VectorSvelte[...] access at ${computedUses.join(', ')}; the dead-export pass cannot see those uses`);
const dead = computedUses.length ? [] : [...exported].filter(n => !svelteUses.has(n) && !COMPONENT_ONLY.has(n)).sort();
if (dead.length) { findings += dead.length; console.log(`index.js exports nothing calls: ${dead.join(', ')}`); }
// A script may deliberately wrap a bundle export under the same name. It is only a trap when
// the shadowing script never calls the export it hides: the two are then different functions,
// and which one a call reaches depends on whether someone typed the prefix.
for (const name of [...exported].sort()) {
    const owners = declaredIn.get(name);
    if (!owners) continue;
    if (owners.some(f => svelteUsesIn.get(f)?.has(name))) continue;
    findings++;
    console.log(`${owners.join(', ')}: declares '${name}', which is also a different VectorSvelte export`);
}
for (const registry of new Set(Object.values(REGISTRY_OF))) {
    if (!registryKeys.has(registry)) { findings++; console.log(`components/lib/shell.svelte.js: could not read the '${registry}' registry's keys`); }
}
for (const { file, line, fn, name } of registryCalls) {
    const keys = registryKeys.get(REGISTRY_OF[fn]);
    if (keys && !keys.has(name)) { findings++; console.log(`${file}:${line}: ${fn}('${name}'), but shell.svelte.js declares no such key in '${REGISTRY_OF[fn]}'`); }
}
if (findings) { console.log(`\n${findings} finding(s)`); process.exit(1); }
console.log(`[check-globals] ${scripts.length} scripts, ${declared.size} declared names, ${exported.size} exports, ${svelteUses.size} used, ${registryCalls.length} registry keys: clean`);
