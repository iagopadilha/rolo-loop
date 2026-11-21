"""Rolo, the mouse in the wheel: the body and feet layers of app/frontend/src/components/rolo_art.js, drawn as ASCII pixel art.

Each character is one 6 px cell of the 200x200 grid (cell 0 starts at 4,4); the wheel (rim, spokes) stays as it is.
  K outline/eye  B body  W belly  P pink (ears, nose, tail, feet)  D ear rim
Run it after editing the drawing, then the brand files:  python tools/rolo_mouse_art.py && python tools/brand_assets.py
"""
import re, sys, pathlib
ROOT = pathlib.Path(__file__).resolve().parent.parent
ART = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'app' / 'frontend' / 'src' / 'components' / 'rolo_art.js'
OUT = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else ART
COL = {'K': '#18181a', 'B': '#b3a9ff', 'W': '#ececef', 'P': '#ff9db1', 'D': '#8b7cf6'}
# body (bounces): columns start at cell X0, rows at cell Y0; facing right
X0, Y0 = 4, 15
BODY = [
    "...........DDD..DDD.....",
    "..........DPPPDDPPPD....",
    "..........DPPPDDPPPD....",
    "...........DBBBBBBD.....",
    "..........BBBBBBBBBB....",
    "....BBBBBBBBBBBBBKBBB...",
    "..BBBBBBBBBBBBBBBBBBBB..",
    ".BBBBBBBBBBBBBBBBBBBBBBP",
    ".BBBBBBBBBBBBBBBBBBBBB..",
    ".BBBBBBBBWWWWWWWBBBB....",
    "..BBBBBBWWWWWWWWBBB.....",
    "...BBBBBBWWWWWBBB.......",
]
TAIL = [  # from the rump, curling up behind the back
    (3, 5), (2, 4), (2, 3), (2, 2), (3, 1), (4, 1),
]
FEET_A = ["...PP.........PP......"]
FEET_B = [".....PP.........PP...."]
FEET_ROW = Y0 + len(BODY)

def rects(rows, x0, y0, legend):
    out = {}
    for r, line in enumerate(rows):
        for c, ch in enumerate(line):
            if ch in legend:
                out.setdefault(COL[ch], []).append(f"{4 + 6 * (x0 + c)} {4 + 6 * (y0 + r)} 6 6")
    return out

def merge(*maps):
    out = {}
    for m in maps:
        for k, v in m.items():
            out.setdefault(k, []).extend(v)
    return out

body = rects(BODY, X0, Y0, 'KBWPD')
tail = {'#ff9db1': [f"{4 + 6 * (X0 + c)} {4 + 6 * (Y0 + r)} 6 6" for c, r in TAIL]}
body = merge(tail, body)
feet_a = rects(FEET_A, X0, FEET_ROW, 'P')
feet_b = rects(FEET_B, X0, FEET_ROW, 'P')

text = ART.read_text(encoding='utf-8')
def layer_js(name, m):
    inner = ''.join(f"\n    '{c}': '{','.join(v)}'," for c, v in m.items())
    return f"  {name}: {{{inner}\n  }}"
for name, m in (('body', body), ('feetA', feet_a), ('feetB', feet_b)):
    text = re.sub(r'  ' + name + r': \{.*?\n  \}', lambda _m, s=layer_js(name, m): s, text, count=1, flags=re.S)
OUT.write_text(text, encoding='utf-8', newline='\n')
print(OUT.relative_to(ROOT) if OUT.is_relative_to(ROOT) else OUT)
