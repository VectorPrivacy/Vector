// The colour an album card's controls wear, measured from its art.

// The art's most prominent colourful hue, lifted to a brightness the controls read at.
export function artAccent(src) {
    return new Promise((resolve) => {
        const img = new Image();
        img.onerror = () => resolve(null);
        img.onload = () => {
            const N = 24, canvas = document.createElement('canvas');
            canvas.width = canvas.height = N;
            const ctx = canvas.getContext('2d', { willReadFrequently: true });
            ctx.drawImage(img, 0, 0, N, N);
            const px = ctx.getImageData(0, 0, N, N).data;
            const bins = Array.from({ length: 12 }, () => ({ w: 0, r: 0, g: 0, b: 0 }));
            let total = 0;
            for (let i = 0; i < px.length; i += 4) {
                const r = px[i] / 255, g = px[i + 1] / 255, b = px[i + 2] / 255;
                const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
                const sat = max === min ? 0 : (max - min) / (1 - Math.abs(2 * l - 1));
                // Near-black and near-white carry no colour worth naming.
                const w = sat * sat * (1 - Math.abs(2 * l - 1));
                total += 1;
                if (w < 0.02) continue;
                let hue = max === r ? ((g - b) / (max - min)) % 6 : max === g ? (b - r) / (max - min) + 2 : (r - g) / (max - min) + 4;
                const bin = bins[Math.floor(((hue * 60 + 360) % 360) / 30)];
                bin.w += w; bin.r += px[i] * w; bin.g += px[i + 1] * w; bin.b += px[i + 2] * w;
            }
            const top = bins.reduce((a, b) => (b.w > a.w ? b : a));
            if (top.w / total < 0.03) { resolve(null); return; }
            const [hh, ss] = toHsl(top.r / top.w, top.g / top.w, top.b / top.w);
            resolve(`hsl(${Math.round(hh)}, ${Math.round(Math.min(0.85, Math.max(0.45, ss)) * 100)}%, 68%)`);
        };
        img.src = src;
    });
}
function toHsl(r, g, b) {
    r /= 255; g /= 255; b /= 255;
    const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
    if (!d) return [0, 0, l];
    const s = d / (1 - Math.abs(2 * l - 1));
    const h = max === r ? ((g - b) / d + 6) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    return [h * 60, s, l];
}
