"""Writes the Rolo Loop brand files from the mascot art (app/frontend/src/components/rolo_art.js):

  crates/desktop/icons/icon.svg      app icon source (then: cd crates/desktop && cargo tauri icon icons/icon.svg -o icons)
  app/frontend/assets/rolo.svg       favicon and static mark of the panel
  docs/media/rolo.svg                README mark
  docs/media/rolo-running.svg        README hero: Rolo running in the wheel (SVG animation, no script)

Run it after any change to the art:  python tools/brand_assets.py
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
ART = ROOT / 'app' / 'frontend' / 'src' / 'components' / 'rolo_art.js'
TILE = '#18181a'
PAD = 16
HUB = (97, 97)
STEPS = 12


def layers():
    text = ART.read_text(encoding='utf-8')
    found = {}
    for name in ('rim', 'spokes', 'body', 'feetA', 'feetB'):
        block = re.search(r'\b' + name + r': \{(.*?)\n  \}', text, re.S).group(1)
        found[name] = re.findall(r"'(#[0-9a-f]{6})': '([^']*)'", block)
    return found


def rects(entries):
    out = []
    for color, items in entries:
        for item in items.split(','):
            x, y, w, h = item.split()
            out.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{color}"/>')
    return ''.join(out)


def svg(art, size, tile=True, running=False, title='Rolo Loop'):
    box = f'{-PAD} {-PAD} {200 + 2 * PAD} {200 + 2 * PAD}' if tile else '0 0 200 200'
    turn = ';'.join(f'{360 / STEPS * i:g} {HUB[0]} {HUB[1]}' for i in range(STEPS + 1))
    spin = f'<animateTransform attributeName="transform" type="rotate" values="{turn}" dur="1.1s" calcMode="discrete" repeatCount="indefinite"/>' if running else ''
    bounce = '<animateTransform attributeName="transform" type="translate" values="0 0;0 -4;0 0;0 -2" dur="0.44s" calcMode="discrete" repeatCount="indefinite"/>' if running else ''
    feet_a = '<animate attributeName="opacity" values="1;0" dur="0.22s" calcMode="discrete" repeatCount="indefinite"/>' if running else ''
    feet_b = '<animate attributeName="opacity" values="0;1" dur="0.22s" calcMode="discrete" repeatCount="indefinite"/>' if running else ''
    tile_rect = f'<rect x="{-PAD}" y="{-PAD}" width="{200 + 2 * PAD}" height="{200 + 2 * PAD}" rx="52" fill="{TILE}"/>' if tile else ''
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{box}" width="{size}" height="{size}" shape-rendering="crispEdges" role="img" aria-label="{title}">'
            f'<title>{title}</title>{tile_rect}<g>{rects(art["rim"])}</g><g>{rects(art["spokes"])}{spin}</g>'
            f'<g>{bounce}{rects(art["body"])}<g>{rects(art["feetA"])}{feet_a}</g><g opacity="{0 if running else 1}">{rects(art["feetB"])}{feet_b}</g></g></svg>\n')


def main():
    art = layers()
    outputs = {
        ROOT / 'crates' / 'desktop' / 'icons' / 'icon.svg': svg(art, 1024),
        ROOT / 'app' / 'frontend' / 'assets' / 'rolo.svg': svg(art, 64),
        ROOT / 'docs' / 'media' / 'rolo.svg': svg(art, 160),
        ROOT / 'docs' / 'media' / 'rolo-running.svg': svg(art, 200, running=True, title='Rolo, the hamster in the wheel'),
    }
    for path, text in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8', newline='\n')
        print(path.relative_to(ROOT))


if __name__ == '__main__':
    main()
