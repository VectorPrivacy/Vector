"""Writes the JPEG corpus tests/stock.rs decodes: one synthetic picture through every encoder
setting Pillow (libjpeg) and ImageMagick offer that changes how a file is laid out.
    python3 make.py [out dir] [--big]
"""
import io, math, os, random, subprocess, sys
from PIL import Image, ImageFile

ImageFile.MAXBLOCK = 1 << 24

out = next((a for a in sys.argv[1:] if not a.startswith('--')), os.path.dirname(os.path.abspath(__file__)))
big = '--big' in sys.argv
random.seed(7)

def picture(w, h):
    img = Image.new('RGB', (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            r = int(127 + 120 * math.sin(x * 0.11) * math.cos(y * 0.07))
            g = int(255 * x / max(w - 1, 1))
            b = 255 if (x // 9 + y // 7) % 2 else int(255 * y / max(h - 1, 1))
            n = random.randint(-8, 8)
            px[x, y] = (max(0, min(255, r + n)), max(0, min(255, g - n)), max(0, min(255, b + n)))
    return img

sizes = [(1, 1), (9, 7), (33, 17), (97, 131)] + ([(640, 480), (1023, 769)] if big else [])
kinds = {
    'base420': dict(subsampling=2),
    'base422': dict(subsampling=1),
    'base444': dict(subsampling=0),
    'prog420': dict(subsampling=2, progressive=True),
    'prog422': dict(subsampling=1, progressive=True),
    'prog444': dict(subsampling=0, progressive=True),
    'opt420': dict(subsampling=2, optimize=True),
    'q100': dict(subsampling=0, quality=100, progressive=True),
    'q5': dict(subsampling=2, quality=5),
    'rst1': dict(subsampling=2, restart_marker_rows=1),
    'rst3p': dict(subsampling=2, restart_marker_blocks=3, progressive=True),
    'rgb': dict(subsampling=0, keep_rgb=True),
    'rgbp': dict(subsampling=0, keep_rgb=True, progressive=True),
}
for (w, h) in sizes:
    img = picture(w, h)
    for name, opts in kinds.items():
        opts = dict(quality=80, **opts) if 'quality' not in opts else opts
        img.save(f'{out}/{name}_{w}x{h}.jpg', 'JPEG', **opts)
    img.convert('L').save(f'{out}/grey_{w}x{h}.jpg', 'JPEG', quality=80)
    img.convert('L').save(f'{out}/greyp_{w}x{h}.jpg', 'JPEG', quality=80, progressive=True)
    img.convert('CMYK').save(f'{out}/cmyk_{w}x{h}.jpg', 'JPEG', quality=80)
    img.convert('CMYK').save(f'{out}/cmykp_{w}x{h}.jpg', 'JPEG', quality=80, progressive=True)
    src = f'{out}/.src.png'
    img.save(src)
    for name, factor, interlace in [('im411', '4x1', 'none'), ('im440', '1x2', 'none'),
                                    ('im440p', '1x2', 'plane'), ('im411p', '4x1', 'plane')]:
        subprocess.run(['convert', src, '-quality', '80', '-sampling-factor', factor,
                        '-interlace', interlace, f'{out}/{name}_{w}x{h}.jpg'], check=True)
    os.remove(src)
