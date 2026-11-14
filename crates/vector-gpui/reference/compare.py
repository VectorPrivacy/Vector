#!/usr/bin/env python3
"""Side-by-side of a Tauri reference render and a vector-gpui render, plus their difference.
   compare.py ref.png gpui.png out.png [crop x,y,w,h]"""
import sys
from PIL import Image, ImageChops, ImageDraw

ref, ours, out = (Image.open(p).convert('RGB') for p in sys.argv[1:3]), None, sys.argv[3]
ref, ours = list(ref)
if len(sys.argv) > 4:
    x, y, w, h = map(int, sys.argv[4].split(','))
    ref, ours = ref.crop((x, y, x + w, y + h)), ours.crop((x, y, x + w, y + h))
ours = ours.resize(ref.size) if ours.size != ref.size else ours
diff = ImageChops.difference(ref, ours).convert('L')
mask = diff.point(lambda v: 255 if v > 24 else 0)
heat = Image.composite(Image.new('RGB', ref.size, (255, 40, 120)), ref.point(lambda v: v // 3), mask)
w, h = ref.size
sheet = Image.new('RGB', (w * 3 + 20, h + 22), (40, 40, 40))
for i, (img, label) in enumerate([(ref, 'Tauri'), (ours, 'GPUI'), (heat, 'diff')]):
    sheet.paste(img, (i * (w + 10), 22))
    ImageDraw.Draw(sheet).text((i * (w + 10) + 4, 4), label, fill=(230, 230, 230))
sheet.save(out)
off = mask.histogram()[255]
print(f'{out}: {100 * off / (w * h):.2f}% of pixels differ by more than 24/255')
