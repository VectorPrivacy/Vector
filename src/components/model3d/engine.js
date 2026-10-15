// The 3D model viewer, bundled apart (src/model3d.bundle.js, global `VectorModel3D`) and loaded
// only when a model first shows. One WebGL context serves every view: a frame renders on the
// shared canvas and is copied onto the view's own, so a chat full of models never runs out of
// contexts. Nothing renders while idle: a frame is drawn on a change, or per tick while an
// animation plays on screen.
import {
    WebGLRenderer, Scene, PerspectiveCamera, PMREMGenerator, Box3, Vector3, Mesh,
    MeshStandardMaterial, AnimationMixer, LoadingManager, ACESFilmicToneMapping, SRGBColorSpace, Timer,
} from 'three';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { STLLoader } from 'three/examples/jsm/loaders/STLLoader.js';
import { OBJLoader } from 'three/examples/jsm/loaders/OBJLoader.js';

export const EXTENSIONS = ['glb', 'gltf', 'stl', 'obj'];

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

// Views whose animation plays, ticked by one loop that stops when none are on screen.
const playing = new Set();
const timer = new Timer();
let loop = 0;
function tick(t) {
    loop = 0;
    timer.update(t);
    const dt = Math.min(timer.getDelta(), 0.1);
    let any = false;
    for (const v of playing) {
        if (!v.onScreen || document.hidden) continue;
        v.mixer.update(dt);
        v.draw();
        any = true;
    }
    if (any) loop = requestAnimationFrame(tick);
}
function wake() {
    if (!loop && playing.size) {
        timer.reset();
        loop = requestAnimationFrame(tick);
    }
}
document.addEventListener('visibilitychange', wake);

/**
 * Show the model at `url` (extension `ext`) on `canvas`. `onState` hears
 * `{ phase: 'loading' | 'ready' | 'error', text?, animated? }`.
 */
export function createView(canvas, { url, ext, onState = () => {} }) {
    const ctx2d = canvas.getContext('2d');
    const scene = new Scene();
    const camera = new PerspectiveCamera(35, 1, 0.01, 1000);
    const controls = new OrbitControls(camera, canvas);
    controls.enableDamping = false;
    controls.enablePan = false;
    // A vertical swipe still scrolls the chat; a sideways drag or a pinch moves the model.
    canvas.style.touchAction = 'pan-y';
    let root = null;
    let disposed = false;
    let frame = 0;

    const view = {
        mixer: null,
        onScreen: true,
        draw() {
            if (disposed || !root) return;
            const { renderer, environment } = gl();
            const dpr = Math.min(window.devicePixelRatio || 1, 2);
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
    const requestDraw = () => {
        if (frame || disposed) return;
        frame = requestAnimationFrame(() => { frame = 0; view.draw(); });
    };
    controls.addEventListener('change', requestDraw);

    // A wheel scrolls the chat; a pinch (ctrl+wheel on a trackpad) zooms the model.
    const onWheel = (e) => { if (!e.ctrlKey) e.stopImmediatePropagation(); };
    canvas.addEventListener('wheel', onWheel, { capture: true });
    const reset = () => { controls.reset(); requestDraw(); };
    canvas.addEventListener('dblclick', reset);
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
        if (disposed) { disposeTree(parsed.root); return; }
        root = parsed.root;
        scene.add(root);

        // Framed from a three-quarter view, whatever its size or origin.
        const box = new Box3().setFromObject(root);
        const center = box.getCenter(new Vector3());
        const radius = Math.max(box.getSize(new Vector3()).length() / 2, 1e-3);
        const distance = radius / Math.sin((camera.fov * Math.PI) / 360) * 1.05;
        camera.near = distance / 100;
        camera.far = distance * 100;
        camera.position.copy(center).add(new Vector3(0.6, 0.45, 1).normalize().multiplyScalar(distance));
        controls.target.copy(center);
        controls.minDistance = radius * 0.2;
        controls.maxDistance = distance * 4;
        controls.update();
        controls.saveState();

        if (parsed.clips.length) {
            view.mixer = new AnimationMixer(root);
            for (const clip of parsed.clips.slice(0, 1)) view.mixer.clipAction(clip).play();
            // Its first frame, not the bind pose, until it plays.
            view.mixer.update(0);
        }
        onState({ phase: 'ready', animated: !!view.mixer });
        requestDraw();
    })().catch((err) => {
        console.warn('[3D] model failed:', err);
        if (!disposed) onState({ phase: 'error', text: friendly(err) });
    });

    return {
        /** Play or pause its first animation; returns whether it plays. */
        setPlaying(on) {
            if (!view.mixer) return false;
            if (on) { playing.add(view); wake(); } else { playing.delete(view); }
            return on;
        },
        reset,
        dispose() {
            disposed = true;
            playing.delete(view);
            if (frame) cancelAnimationFrame(frame);
            ro.disconnect();
            io.disconnect();
            canvas.removeEventListener('wheel', onWheel, { capture: true });
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
