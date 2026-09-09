#!/usr/bin/env node
/**
 * check-globals.mjs — the one-global-scope frontend's missing-name check.
 *
 * The plain scripts in index.html share one scope, so a global one file deletes is a
 * ReferenceError in another file at the moment that path runs, not at load. This parses
 * every first-party script, collects every name any of them declares (any depth), and
 * reports identifiers that none declares and the browser does not provide. It also checks
 * that every `VectorSvelte.<name>` the scripts use is exported by src/components/index.js.
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
const scripts = [...html.matchAll(/<script src="\/?([^"]+)"/g)].map(m => m[1]).filter(p => !VENDORED.test(p));

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
    'OfflineAudioContext', 'AnalyserNode', 'GainNode', 'Worker', 'WebSocket', 'XMLHttpRequest', 'DOMParser', 'XMLSerializer', 'Range', 'Selection',
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
const perFile = new Map();           // file → Set of identifiers referenced
const svelteUses = new Map();        // VectorSvelte.<name> → [files]

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
    const ast = acorn.parse(code, { ecmaVersion: 'latest', sourceType: 'script', allowHashBang: true });
    const refs = new Set();
    walk(ast, (n) => {
        switch (n.type) {
            case 'VariableDeclarator': collectPattern(n.id, declared); break;
            case 'FunctionDeclaration': case 'FunctionExpression': case 'ArrowFunctionExpression':
                if (n.id) declared.add(n.id.name);
                for (const p of n.params) collectPattern(p, declared);
                break;
            case 'ClassDeclaration': case 'ClassExpression': if (n.id) declared.add(n.id.name); break;
            case 'CatchClause': collectPattern(n.param, declared); break;
            case 'ImportDeclaration': for (const s of n.specifiers) declared.add(s.local.name); break;
            case 'MemberExpression':
                if (n.object.type === 'Identifier' && n.object.name === 'VectorSvelte' && !n.computed && n.property.type === 'Identifier') {
                    const list = svelteUses.get(n.property.name) || [];
                    list.push(rel);
                    svelteUses.set(n.property.name, list);
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
        if ((n.type === 'BreakStatement' || n.continueStatement) && n.label) n.label.__notRef = true;
        if (n.type === 'Identifier' && !n.__notRef) refs.add(n.name);
    });
    perFile.set(rel, refs);
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

let findings = 0;
for (const [rel, refs] of perFile) {
    const missing = [...refs].filter(n => !declared.has(n) && !PROVIDED.has(n)).sort();
    if (missing.length) { findings += missing.length; console.log(`${rel}: undeclared ${missing.join(', ')}`); }
}
for (const [name, files] of svelteUses) {
    if (!exported.has(name)) { findings++; console.log(`VectorSvelte.${name} is not exported (used in ${[...new Set(files)].join(', ')})`); }
}
if (findings) { console.log(`\n${findings} finding(s)`); process.exit(1); }
console.log(`[check-globals] ${scripts.length} scripts, ${declared.size} declared names, ${svelteUses.size} VectorSvelte members: clean`);
