#!/usr/bin/env python3
"""Derive the icon files from packaging/icons/merlin.png (128x128 pixel art), always with nearest-neighbour scaling.

Writes the raw RGBA the window icon embeds, the 256px icon for the icon theme and the landing page's favicons.
Needs Pillow:  tools/make-icon.py
"""
import os
from PIL import Image

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
src = Image.open(os.path.join(root, "packaging/icons/merlin.png")).convert("RGBA")
assert src.size == (128, 128), src.size

def scaled(size):
    return src.resize((size, size), Image.NEAREST)

open(os.path.join(root, "app/data/icon-128.rgba"), "wb").write(src.tobytes())
scaled(256).save(os.path.join(root, "packaging/icons/merlin-256.png"))
public = os.path.join(root, "site/public")
src.save(os.path.join(public, "logo.png"))
scaled(32).save(os.path.join(public, "favicon-32.png"))
scaled(256).save(os.path.join(public, "apple-touch-icon.png"))
