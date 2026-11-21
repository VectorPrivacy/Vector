# Builds assets/fonts/VectorSlot.ttf: python3 scripts/slot-font.py assets/fonts/Rubik-Regular.ttf assets/fonts/VectorSlot.ttf
import sys
from fontTools.ttLib import TTFont
from fontTools.subset import Subsetter, Options
from fontTools.pens.ttGlyphPen import TTGlyphPen
src, dst = sys.argv[1], sys.argv[2]
f = TTFont(src)
o = Options(); o.name_IDs = ['*']; o.notdef_outline = True; o.layout_features = []; o.hinting = False
s = Subsetter(o); s.populate(unicodes=[ord('m')]); s.subset(f)
upm = f['head'].unitsPerEm
glyf, hmtx = f['glyf'], f['hmtx']
order = f.getGlyphOrder()
for name, cp, adv in [('uni2003', 0x2003, upm), ('uni2005', 0x2005, upm // 4)]:
    glyf.glyphs[name] = TTGlyphPen(None).glyph()
    hmtx.metrics[name] = (adv, 0)
    order.append(name)
    for t in f['cmap'].tables:
        if t.isUnicode(): t.cmap[cp] = name
f.setGlyphOrder(order)
f['maxp'].numGlyphs = len(order)
name = f['name']
for rec in list(name.names):
    if rec.nameID in (1, 16): rec.string = 'Vector Slot'
    elif rec.nameID == 4: rec.string = 'Vector Slot Regular'
    elif rec.nameID == 6: rec.string = 'VectorSlot-Regular'
    elif rec.nameID == 3: rec.string = 'VectorSlot-Regular'
f.save(dst)
