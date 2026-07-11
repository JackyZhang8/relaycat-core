#!/usr/bin/env python3
"""Generate a macOS-style rounded (squircle) app-icon source from the square
cat art. macOS does not round app icons automatically, so the rounding +
margin must be baked into the icon. Output is fed to `cargo tauri icon`.
"""
import sys
from PIL import Image

SRC = sys.argv[1] if len(sys.argv) > 1 else "../icons/app_1024x1024.png"
OUT = sys.argv[2] if len(sys.argv) > 2 else "/tmp/relaycat-icon-source.png"

CANVAS = 1024
BODY = 824          # Apple macOS icon grid: art body within the 1024 canvas
MARGIN = (CANVAS - BODY) // 2
SS = 4              # supersample factor for smooth edges
N = 5.0            # superellipse exponent (~Apple squircle)


def squircle_mask(size: int, n: float) -> Image.Image:
    big = size * SS
    mask = Image.new("L", (big, big), 0)
    px = mask.load()
    a = big / 2.0
    for y in range(big):
        ny = (y + 0.5 - a) / a
        nyn = abs(ny) ** n
        for x in range(big):
            nx = (x + 0.5 - a) / a
            if abs(nx) ** n + nyn <= 1.0:
                px[x, y] = 255
    return mask.resize((size, size), Image.LANCZOS)


def main() -> None:
    art = Image.open(SRC).convert("RGBA").resize((BODY, BODY), Image.LANCZOS)
    mask = squircle_mask(BODY, N)
    # Combine the existing alpha with the squircle mask.
    r, g, b, a = art.split()
    from PIL import ImageChops
    art.putalpha(ImageChops.multiply(a, mask))
    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    canvas.paste(art, (MARGIN, MARGIN), art)
    canvas.save(OUT)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
