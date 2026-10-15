// The 3D viewer (src/model3d.bundle.js) loads on the first model a chat shows, never at start.
export const MODEL_EXTENSIONS = ['glb', 'gltf', 'stl', 'obj'];

let engine = null;
export function loadModelEngine() {
    engine ??= new Promise((resolve, reject) => {
        if (window.VectorModel3D) return resolve(window.VectorModel3D);
        const s = document.createElement('script');
        s.src = 'model3d.bundle.js';
        s.onload = () => (window.VectorModel3D ? resolve(window.VectorModel3D) : reject(new Error('3D viewer missing')));
        s.onerror = () => { engine = null; reject(new Error('3D viewer failed to load')); };
        document.head.appendChild(s);
    });
    return engine;
}
