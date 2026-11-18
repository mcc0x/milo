"""Regenerate figures.alpha with Python and Pillow; see README.md."""

from io import BytesIO
from pathlib import Path
from struct import pack
from urllib.request import urlopen
from zlib import compress, crc32

from PIL import Image, ImageDraw, ImageFont

FONT = "https://raw.githubusercontent.com/rsms/inter/v4.1/docs/font-files/InterVariable.ttf"
GLYPHS = "0123456789<+%"
HEIGHT, PITCH, PERCENT = 64, 38, 25
BASELINE = 56


def image_data(image):
    """PNG IDAT chunk with two 4-bit alpha indices per byte."""
    raw = bytearray()
    pixels = image.tobytes()
    for y in range(image.height):
        row = pixels[y * image.width:(y + 1) * image.width]
        if len(row) % 2:
            row += b"\0"
        raw.append(0)  # PNG scanline filter: none.
        raw.extend((((a + 8) // 17) << 4) | ((b + 8) // 17) for a, b in zip(row[::2], row[1::2]))
    data = b"IDAT" + compress(raw, 9)
    return pack(">I", len(data) - 4) + data + pack(">I", crc32(data))


def main():
    with urlopen(FONT) as response:
        font_bytes = response.read()
    masks = {}
    for glyph in GLYPHS:
        small = glyph == "%"
        # Tabular digits; smaller operators, centred on the digits.
        size = 27 if small else 49 if glyph in "<+" else 60
        font = ImageFont.truetype(BytesIO(font_bytes), size)
        font.set_variation_by_axes([24, 750])  # Optical size, then weight.
        width = PERCENT if small else PITCH
        ink = Image.new("L", (80, HEIGHT))
        ImageDraw.Draw(ink).text((4, BASELINE), glyph, font=font, fill=255, anchor="ls")
        left, top, right, bottom = ink.getbbox()
        assert right - left <= width, glyph
        mask = Image.new("L", (width, HEIGHT))
        x = (width - (right - left)) // 2
        y = 34 - (bottom - top) // 2 if glyph in "<+" else top
        mask.paste(ink.crop((left, top, right, bottom)), (x, y))
        masks[glyph] = mask

    # Offsets (big-endian u32), then complete IDAT chunks. The app adds the
    # PNG header and its chosen palette without touching the compressed pixels.
    atlas = bytearray()
    offsets = bytearray()
    for figure in [f"{n}%" for n in range(101)] + ["<1%", "99+%"]:
        offsets.extend(pack(">I", len(atlas)))
        image = Image.new("L", ((len(figure) - 1) * PITCH + PERCENT, HEIGHT))
        for i, glyph in enumerate(figure):
            image.paste(masks[glyph], (i * PITCH, 0))
        atlas.extend(image_data(image))
    offsets.extend(pack(">I", len(atlas)))
    Path(__file__).with_name("figures.alpha").write_bytes(offsets + atlas)


if __name__ == "__main__":
    main()
