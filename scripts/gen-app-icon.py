"""Erzeugt das App-Master-Icon (1024×1024 PNG) für Streichzeug.

Tailwind-Blau (#2563EB) Hintergrund mit weißem „S" zentriert. Stil
bleibt konsistent zum bisherigen Clipboard-PII-Icon — gleiche Farbe,
gleiche rounded-square-Geometrie, gleiche Bold-Sans-Typografie.

Output: src-tauri/icons/master-icon.png

Folgeschritt: `cargo tauri icon src-tauri/icons/master-icon.png` ruft
die Tauri-CLI auf, die daraus alle plattform-spezifischen Größen
(32/64/128/Square*/icon.icns/icon.ico) generiert.

Aufruf:
    python3 scripts/gen-app-icon.py
"""
import pathlib
from PIL import Image, ImageDraw, ImageFont

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DST = REPO_ROOT / "src-tauri" / "icons" / "master-icon.png"

SIZE = 1024
BG_COLOR = (37, 99, 235, 255)  # Tailwind blue-600 (bisheriges Icon)
FG_COLOR = (255, 255, 255, 255)
CORNER_RADIUS = int(SIZE * 0.225)  # iOS-Style abgerundete Ecken

# macOS-Schriftarten in Order der Bevorzugung. Bold + Sans-Serif für
# klaren Markenauftritt in der Menubar/Dock/Taskleiste.
FONT_CANDIDATES = [
    "/System/Library/Fonts/Helvetica.ttc",
    "/System/Library/Fonts/HelveticaNeue.ttc",
    "/System/Library/Fonts/SFNS.ttf",
    "/Library/Fonts/Arial Bold.ttf",
]

def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if pathlib.Path(path).exists():
            try:
                # Bold-Variant via index probieren (Helvetica.ttc enthält
                # mehrere Schnitte unter index 0..4).
                return ImageFont.truetype(path, size=size, index=1)
            except (OSError, IndexError):
                try:
                    return ImageFont.truetype(path, size=size)
                except OSError:
                    continue
    # Fallback: PIL-Default-Bitmap (klein, nur als Notnagel)
    return ImageFont.load_default()


def main() -> None:
    canvas = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)

    # 1) Rounded square background
    draw.rounded_rectangle(
        ((0, 0), (SIZE, SIZE)),
        radius=CORNER_RADIUS,
        fill=BG_COLOR,
    )

    # 2) Weißes „S" zentriert
    font = load_font(int(SIZE * 0.70))
    bbox = draw.textbbox((0, 0), "S", font=font)
    text_w = bbox[2] - bbox[0]
    text_h = bbox[3] - bbox[1]
    # Den Bounding-Box-Offset herausrechnen, damit das S optisch zentriert
    # sitzt (Schriften haben nicht-null y-Origins).
    x = (SIZE - text_w) // 2 - bbox[0]
    y = (SIZE - text_h) // 2 - bbox[1]
    draw.text((x, y), "S", fill=FG_COLOR, font=font)

    canvas.save(DST)
    print(f"wrote {DST.relative_to(REPO_ROOT)}: {canvas.size}")


if __name__ == "__main__":
    main()
