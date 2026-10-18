#!/usr/bin/env python3
"""Writes Android's fonts.xml with Marathon's UI face (Source Sans 3, which
stands in for Sail Sans Pro) as "sans-serif", Android's default family.

The input is the Waydroid image's /system/etc/fonts.xml; only the sans-serif
family changes. The font files come from the host's
/usr/share/fonts/source-sans-3 (copied into /system/fonts at container start).

Usage: gen-fonts-xml.py IMAGE_FONTS_XML fonts.xml
"""
import re
import sys

# weight -> (normal file, italic file or None). Source Sans 3 has no Medium
# or Black: 500 takes Semibold (Material's titles and buttons), 800+ Bold.
FACES = {
    100: ('SourceSans3-ExtraLight.ttf', None),
    200: ('SourceSans3-ExtraLight.ttf', None),
    300: ('SourceSans3-Light.ttf', 'SourceSans3-LightIt.ttf'),
    400: ('SourceSans3-Regular.ttf', 'SourceSans3-It.ttf'),
    500: ('SourceSans3-Semibold.ttf', None),
    600: ('SourceSans3-Semibold.ttf', None),
    700: ('SourceSans3-Bold.ttf', None),
    800: ('SourceSans3-Bold.ttf', None),
    900: ('SourceSans3-Bold.ttf', None),
}


def family():
    lines = ['    <family name="sans-serif">']
    for weight, (normal, _) in FACES.items():
        lines.append('        <font weight="%d" style="normal">%s</font>' % (weight, normal))
    for weight, (_, italic) in FACES.items():
        if italic:
            lines.append('        <font weight="%d" style="italic">%s</font>' % (weight, italic))
    lines.append('    </family>')
    return '\n'.join(lines)


def main():
    src, out = sys.argv[1], sys.argv[2]
    xml = open(src).read()
    pattern = re.compile(r'    <family name="sans-serif">.*?</family>', re.S)
    if len(pattern.findall(xml)) != 1:
        sys.exit('expected exactly one sans-serif family in %s' % src)
    xml = pattern.sub(lambda _: family(), xml)
    with open(out, 'w') as f:
        f.write(xml)


if __name__ == '__main__':
    main()
