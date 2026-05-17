"""Regeneriert das macOS-Tray-Template-Icon aus dem App-Master-Icon.

Quelle: src-tauri/icons/master-icon.png (blaue Kachel mit weißem „S")
Ziel:   src-tauri/icons/tray-icon.png (32×32, schwarze S-Silhouette
        auf transparentem Grund — Template-Image)

Hintergrund: macOS Tray-Icons sollten Template-Images sein. Das System
färbt sie dann passend zur Menubar (Dark/Light Mode). Konkret: nur
die Alpha-Information wird genutzt, RGB wird ignoriert. Das App-Logo
direkt zu nehmen würde eine flache schwarze Kachel ergeben — wir
brauchen also die S-Silhouette **isoliert**.

Algorithmus: aus dem Quell-Logo werden alle weißen Pixel (die das „S"
zeichnen) als schwarz mit gleicher Alpha-Intensität übernommen,
alle bläulichen Pixel werden vollständig transparent. Der Schwellwert
beim min(R,G,B) > 80 trennt die Farbpaare zuverlässig.

Aufruf:
    python3 scripts/gen-tray-icon.py
"""
import pathlib
from PIL import Image

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = REPO_ROOT / "src-tauri" / "icons" / "master-icon.png"
DST = REPO_ROOT / "src-tauri" / "icons" / "tray-icon.png"

src = Image.open(SRC).convert("RGBA")
w, h = src.size
out = Image.new("RGBA", (w, h), (0, 0, 0, 0))

for y in range(h):
    for x in range(w):
        r, g, b, a = src.getpixel((x, y))
        if a == 0:
            continue
        # Whiteness = niedrigster RGB-Wert (weiß: 255, blau: ~37).
        # Schwellwert 80 trennt das „S" sauber vom Hintergrund.
        whiteness = min(r, g, b)
        if whiteness > 80:
            new_a = int((whiteness - 80) / (255 - 80) * 255)
            new_a = max(0, min(255, new_a))
            out.putpixel((x, y), (0, 0, 0, new_a))

# Downsample auf 32×32 mit Lanczos für saubere Kanten in der Menubar.
tray = out.resize((32, 32), Image.LANCZOS)
tray.save(DST)
print(f"wrote {DST.relative_to(REPO_ROOT)}: {tray.size}")
