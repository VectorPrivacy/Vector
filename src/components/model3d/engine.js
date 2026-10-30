// The 3D model viewer, bundled apart (src/model3d.bundle.js, global `VectorModel3D`) and loaded
// only when a model first shows. One WebGL context serves every view: a frame renders on the
// shared canvas and is copied onto the view's own, so a chat full of models never runs out of
// contexts. Nothing renders while idle: a frame is drawn on a change, or per tick while an
// animation plays on screen.
import {
    WebGLRenderer, Scene, PerspectiveCamera, PMREMGenerator, Box3, Vector3, Quaternion, Mesh, Group,
    MeshStandardMaterial, AnimationMixer, LoadingManager, ACESFilmicToneMapping, SRGBColorSpace, Timer,
} from 'three';
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { STLLoader } from 'three/examples/jsm/loaders/STLLoader.js';
import { OBJLoader } from 'three/examples/jsm/loaders/OBJLoader.js';
import { FBXLoader } from 'three/examples/jsm/loaders/FBXLoader.js';

export const EXTENSIONS = ['glb', 'gltf', 'stl', 'obj', 'fbx'];

let shared = null;
function gl() {
    if (shared) return shared;
    const renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: 'low-power' });
    renderer.toneMapping = ACESFilmicToneMapping;
    renderer.outputColorSpace = SRGBColorSpace;
    renderer.setClearColor(0x000000, 0);
    const pmrem = new PMREMGenerator(renderer);
    const environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
    pmrem.dispose();
    shared = { renderer, environment };
    return shared;
}

// A model is one file: anything it names outside itself (a .gltf's sibling buffers, a remote
// texture) is never fetched.
function sealedManager() {
    const manager = new LoadingManager();
    manager.setURLModifier((url) => (/^(data|blob):/.test(url) ? url : 'data:,'));
    return manager;
}

// FBX's Phong and Lambert ignore the environment light the viewer uses and would render almost
// black: each becomes the PBR material glTF has, with its colours and maps kept.
const CARRIED = ['color', 'map', 'normalMap', 'normalScale', 'emissive', 'emissiveMap', 'emissiveIntensity',
    'alphaMap', 'aoMap', 'aoMapIntensity', 'bumpMap', 'bumpScale', 'displacementMap', 'displacementScale',
    'transparent', 'opacity', 'side', 'vertexColors', 'flatShading', 'alphaTest', 'name'];
function physical(m) {
    if (!m?.isMeshPhongMaterial && !m?.isMeshLambertMaterial) return m;
    const next = new MeshStandardMaterial({ metalness: 0, roughness: 0.8 });
    for (const k of CARRIED) {
        if (m[k] === undefined || m[k] === null) continue;
        if (m[k]?.isColor || m[k]?.isVector2) next[k].copy(m[k]);
        else next[k] = m[k];
    }
    // Shininess 0..100 maps onto roughness; Phong's specular colour has no PBR twin.
    if (m.isMeshPhongMaterial) next.roughness = Math.min(1, Math.max(0.15, 1 - Math.sqrt((m.shininess ?? 30) / 100)));
    m.dispose();
    return next;
}

function plain() {
    return new MeshStandardMaterial({ color: 0xb8b8c0, metalness: 0.1, roughness: 0.55 });
}

async function parse(buffer, ext) {
    const manager = sealedManager();
    if (ext === 'glb' || ext === 'gltf') {
        const loader = new GLTFLoader(manager);
        const gltf = await new Promise((resolve, reject) => loader.parse(buffer, '', resolve, reject));
        return { root: gltf.scene || gltf.scenes?.[0], clips: gltf.animations || [] };
    }
    if (ext === 'stl') {
        const geometry = new STLLoader(manager).parse(buffer);
        geometry.computeVertexNormals();
        const material = plain();
        if (geometry.hasColors) material.vertexColors = true;
        return { root: new Mesh(geometry, material), clips: [] };
    }
    if (ext === 'fbx') {
        const root = new FBXLoader(manager).parse(buffer, '');
        root.traverse((o) => {
            if (!o.isMesh) return;
            o.material = Array.isArray(o.material) ? o.material.map(physical) : physical(o.material);
        });
        return { root, clips: root.animations || [] };
    }
    if (ext === 'obj') {
        const root = new OBJLoader(manager).parse(new TextDecoder().decode(buffer));
        // No .mtl travels with it: every surface gets one clean material.
        root.traverse((o) => { if (o.isMesh) o.material = plain(); });
        return { root, clips: [] };
    }
    throw new Error('Unsupported model format');
}

function friendly(err) {
    const text = String(err?.message || err);
    if (/draco|meshopt|ktx2|basisu|KHR_texture_basisu|EXT_meshopt/i.test(text)) return "This model is compressed in a way Vector can't open yet.";
    if (/unsupported/i.test(text)) return "Vector can't open this kind of model.";
    if (/FBX version|version not supported/i.test(text)) return "This FBX is too old to open. Re-export it as FBX 2011 or newer.";
    return "This model couldn't be opened.";
}

function disposeTree(root) {
    root.traverse((o) => {
        o.geometry?.dispose();
        const mats = Array.isArray(o.material) ? o.material : o.material ? [o.material] : [];
        for (const m of mats) {
            for (const v of Object.values(m)) if (v?.isTexture) v.dispose();
            m.dispose();
        }
    });
}

const SPIN_PER_PX = 0.01;
const SPIN_DECAY = 0.0045;   // per ms: a flick coasts for about half a second

/**
 * Free rotation: a drag turns the model itself about the view's own axes, so any side can face
 * the camera; a flick keeps it turning briefly. A pinch (or ctrl+wheel, a trackpad's pinch) zooms.
 */
function turntable(canvas, camera, pivot, redraw, touched, { tapped = null, freeWheel = false } = {}) {
    const pointers = new Map();
    let travel = 0;
    let pressed = 0;
    let pinch = 0;
    let velocity = { x: 0, y: 0, t: 0 };
    let coast = 0;
    let distance = 1;
    let limits = [0.2, 4];
    let home = { q: new Quaternion(), distance: 1 };
    const axisX = new Vector3();
    const axisY = new Vector3();
    const q = new Quaternion();

    const turn = (dx, dy) => {
        axisX.setFromMatrixColumn(camera.matrixWorld, 0);
        axisY.setFromMatrixColumn(camera.matrixWorld, 1);
        pivot.quaternion.premultiply(q.setFromAxisAngle(axisY, dx * SPIN_PER_PX));
        pivot.quaternion.premultiply(q.setFromAxisAngle(axisX, dy * SPIN_PER_PX));
        redraw();
    };
    const place = () => {
        camera.position.setLength(distance);
        camera.near = distance / 100;
        camera.far = distance * 100;
        camera.updateProjectionMatrix();
        redraw();
    };
    const zoom = (f) => {
        distance = Math.min(Math.max(distance * f, limits[0]), limits[1]);
        place();
    };
    const stop = () => { if (coast) cancelAnimationFrame(coast); coast = 0; };
    const glide = (t0) => (t) => {
        const dt = Math.min(t - t0, 50);
        const k = Math.exp(-SPIN_DECAY * dt);
        velocity.x *= k;
        velocity.y *= k;
        if (Math.hypot(velocity.x, velocity.y) < 0.02) { coast = 0; return; }
        turn(velocity.x * dt, velocity.y * dt);
        coast = requestAnimationFrame(glide(t));
    };

    const down = (e) => {
        stop();
        if (pointers.size === 0) { travel = 0; pressed = e.timeStamp; }
        canvas.setPointerCapture?.(e.pointerId);
        pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
        velocity = { x: 0, y: 0, t: e.timeStamp };
        if (pointers.size === 2) {
            const [a, b] = [...pointers.values()];
            pinch = Math.hypot(a.x - b.x, a.y - b.y);
        }
    };
    const move = (e) => {
        const p = pointers.get(e.pointerId);
        if (!p) return;
        const dx = e.clientX - p.x;
        const dy = e.clientY - p.y;
        p.x = e.clientX;
        p.y = e.clientY;
        travel += Math.hypot(dx, dy);
        // A press only takes the model over once it moves: a tap is a tap.
        if (travel > 4) touched();
        if (pointers.size === 1) {
            const dt = Math.max(e.timeStamp - velocity.t, 1);
            velocity = { x: dx / dt, y: dy / dt, t: e.timeStamp };
            turn(dx, dy);
        } else if (pointers.size === 2) {
            const [a, b] = [...pointers.values()];
            const d = Math.hypot(a.x - b.x, a.y - b.y);
            if (pinch && d) zoom(pinch / d);
            pinch = d;
        }
    };
    const up = (e) => {
        if (!pointers.delete(e.pointerId)) return;
        pinch = 0;
        if (pointers.size === 0 && e.type === 'pointerup' && travel <= 4 && e.timeStamp - pressed < 500) tapped?.();
        // A flick still moving when released coasts; a drag that stopped first doesn't.
        if (pointers.size === 0 && e.type === 'pointerup' && e.timeStamp - velocity.t < 60) {
            coast = requestAnimationFrame(glide(performance.now()));
        }
    };
    // In a chat a plain wheel scrolls it and ctrl+wheel (a trackpad pinch) zooms; full screen,
    // any wheel zooms.
    const wheel = (e) => {
        if (!e.ctrlKey && !freeWheel) return;
        e.preventDefault();
        touched();
        zoom(Math.exp(e.deltaY * 0.01));
    };
    canvas.addEventListener('pointerdown', down);
    canvas.addEventListener('pointermove', move);
    canvas.addEventListener('pointerup', up);
    canvas.addEventListener('pointercancel', up);
    canvas.addEventListener('wheel', wheel, { passive: false });

    return {
        frame(radius, fit) {
            distance = fit;
            limits = [radius * 1.05, fit * 4];
            home = { q: pivot.quaternion.clone(), distance: fit };
            place();
        },
        reset() {
            stop();
            pivot.quaternion.copy(home.q);
            distance = home.distance;
            place();
        },
        dispose() {
            stop();
            canvas.removeEventListener('pointerdown', down);
            canvas.removeEventListener('pointermove', move);
            canvas.removeEventListener('pointerup', up);
            canvas.removeEventListener('pointercancel', up);
            canvas.removeEventListener('wheel', wheel);
        },
    };
}

// The pedestal: an untouched model turns slowly about its vertical axis, drawn at 30 fps.
const PEDESTAL_SPEED = 0.35;   // rad/s, a turn in about 18 s
const PEDESTAL_FRAME = 1 / 30;
const UP = new Vector3(0, 1, 0);
const calm = window.matchMedia?.('(prefers-reduced-motion: reduce)');

// Views that move on their own (an animation playing, a pedestal turning), ticked by one loop
// that stops when none is on screen.
const ticking = new Set();
// While one is full screen, the views behind it hold still.
let front = null;
const timer = new Timer();
let loop = 0;
function tick(t) {
    loop = 0;
    timer.update(t);
    const dt = Math.min(timer.getDelta(), 0.1);
    let any = false;
    for (const v of ticking) {
        if (!v.onScreen || document.hidden || !document.hasFocus() || (front && v !== front)) continue;
        any = true;
        if (v.animating) v.mixer.update(dt);
        if (v.spinning) v.spun += dt;
        if (!v.animating && v.spun < PEDESTAL_FRAME) continue;
        if (v.spinning) v.pivot.rotateOnWorldAxis(UP, v.spun * PEDESTAL_SPEED);
        v.spun = 0;
        v.draw();
    }
    if (any) loop = requestAnimationFrame(tick);
}
function wake() {
    if (!loop && ticking.size) {
        timer.reset();
        loop = requestAnimationFrame(tick);
    }
}
function schedule(v) {
    if (v.animating || v.spinning) ticking.add(v);
    else ticking.delete(v);
    wake();
}
document.addEventListener('visibilitychange', wake);
window.addEventListener('focus', wake);

/**
 * Show the model at `url` (extension `ext`) on `canvas`. `onState` hears
 * `{ phase: 'loading' | 'ready' | 'error', text?, animated? }`.
 */
export function createView(canvas, { url, ext, onState = () => {}, onTap = null, fullscreen = false }) {
    const ctx2d = canvas.getContext('2d');
    const scene = new Scene();
    const camera = new PerspectiveCamera(35, 1, 0.01, 1000);
    // The camera looks at the origin from a three-quarter view; the model turns, not the camera.
    camera.position.set(0.6, 0.45, 1).normalize();
    camera.lookAt(0, 0, 0);
    camera.updateMatrixWorld();
    const pivot = new Group();
    scene.add(pivot);
    let view = null;
    // In a chat a vertical swipe still scrolls it; full screen, every gesture is the model's.
    canvas.style.touchAction = fullscreen ? 'none' : 'pan-y';
    let root = null;
    let disposed = false;
    let frame = 0;

    view = {
        mixer: null,
        actions: null,
        clip: 0,
        pivot,
        animating: false,
        spinning: false,
        spun: 0,
        onScreen: true,
        draw() {
            if (disposed || !root) return;
            const { renderer, environment } = gl();
            // Sharp up to about 4 MP: a full screen at 2x would otherwise be 16.
            const css = canvas.clientWidth * canvas.clientHeight;
            const dpr = Math.min(window.devicePixelRatio || 1, 2, Math.sqrt(4e6 / Math.max(css, 1)));
            const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
            const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
            if (canvas.width !== w || canvas.height !== h) {
                canvas.width = w;
                canvas.height = h;
            }
            camera.aspect = w / h;
            camera.updateProjectionMatrix();
            scene.environment = environment;
            renderer.setSize(w, h, false);
            renderer.render(scene, camera);
            ctx2d.clearRect(0, 0, w, h);
            ctx2d.drawImage(renderer.domElement, 0, 0);
        },
    };
    if (fullscreen) front = view;
    const requestDraw = () => {
        if (frame || disposed) return;
        frame = requestAnimationFrame(() => { frame = 0; view.draw(); });
    };
    const pedestal = (on) => {
        view.spinning = on && !calm?.matches;
        schedule(view);
    };
    const controls = turntable(canvas, camera, pivot, requestDraw, () => pedestal(false), { tapped: onTap, freeWheel: fullscreen });
    const reset = () => { controls.reset(); pedestal(true); };
    if (fullscreen) canvas.addEventListener('dblclick', reset);
    const ro = new ResizeObserver(requestDraw);
    ro.observe(canvas);
    const io = new IntersectionObserver(([e]) => {
        view.onScreen = e.isIntersecting;
        if (view.onScreen) { requestDraw(); wake(); }
    });
    io.observe(canvas);

    onState({ phase: 'loading' });
    (async () => {
        const res = await fetch(url);
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const parsed = await parse(await res.arrayBuffer(), ext);
        // Exporters leave empty takes behind (FBX's "Take 001"): nothing to play.
        parsed.clips = parsed.clips.filter((c) => c.duration > 0);
        if (disposed) { disposeTree(parsed.root); return; }
        root = parsed.root;
        // Centred on the pivot, and framed whatever its size or origin.
        const box = new Box3().setFromObject(root);
        const center = box.getCenter(new Vector3());
        const radius = Math.max(box.getSize(new Vector3()).length() / 2, 1e-3);
        root.position.sub(center);
        pivot.add(root);
        // Fits the narrower side: a tall phone screen frames by its width.
        const vfov = (camera.fov * Math.PI) / 180;
        const aspect = canvas.clientWidth / Math.max(canvas.clientHeight, 1) || 1;
        const fov = Math.min(vfov, 2 * Math.atan(Math.tan(vfov / 2) * aspect));
        controls.frame(radius, radius / Math.sin(fov / 2) * 1.05);

        if (parsed.clips.length) {
            view.mixer = new AnimationMixer(root);
            view.actions = parsed.clips.map((c) => view.mixer.clipAction(c));
            view.actions[0].play();
            // Its first frame, not the bind pose, until it plays.
            view.mixer.update(0);
        }
        const clips = parsed.clips.map((c, i) => ({ name: c.name || `Animation ${i + 1}`, duration: c.duration }));
        onState({ phase: 'ready', animated: !!view.mixer, clips });
        requestDraw();
        pedestal(true);
    })().catch((err) => {
        console.warn('[3D] model failed:', err);
        if (!disposed) onState({ phase: 'error', text: friendly(err) });
    });

    return {
        /** Play or pause its first animation; returns whether it plays. */
        setPlaying(on) {
            if (!view.mixer) return false;
            view.animating = on;
            schedule(view);
            return on;
        },
        /** Switch to animation `i`, from its start. */
        setClip(i) {
            const next = view.actions?.[i];
            if (!next || next === view.actions[view.clip]) return;
            view.actions[view.clip].stop();
            view.clip = i;
            next.reset().play();
            view.mixer.update(0);
            requestDraw();
        },
        /** Jump to `t` seconds into the animation. */
        seek(t) {
            const a = view.actions?.[view.clip];
            if (!a) return;
            a.time = Math.max(0, Math.min(t, a.getClip().duration));
            view.mixer.update(0);
            requestDraw();
        },
        setSpeed(k) { if (view.mixer) view.mixer.timeScale = k; },
        /** Where the animation is, in seconds. */
        time() {
            const a = view.actions?.[view.clip];
            return a ? a.time % (a.getClip().duration || 1) : 0;
        },
        reset,
        dispose() {
            disposed = true;
            ticking.delete(view);
            if (front === view) { front = null; wake(); }
            if (frame) cancelAnimationFrame(frame);
            ro.disconnect();
            io.disconnect();
            canvas.removeEventListener('dblclick', reset);
            controls.dispose();
            view.mixer?.stopAllAction();
            if (root) disposeTree(root);
            scene.clear();
        },
    };
}

export function supports(ext) {
    return EXTENSIONS.includes(String(ext || '').toLowerCase());
}

// A browser without WebGL shows the file as a file.
export function available() {
    try {
        return !!gl();
    } catch {
        return false;
    }
}
