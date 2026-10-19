#!/usr/bin/env python3
"""Writes Android's fonts.xml with Marathon's font set, and the lists
deckard-fonts-sync needs.

Marathon's faces (what the shell and its apps use, from the host's
/usr/share/fonts): Source Sans 3 for UI text (it stands in for Sail Sans Pro),
JetBrains Mono for code, fontconfig's Noto Serif for serif, Noto Sans and
DejaVu Sans for the scripts Source Sans lacks (fontconfig's fallbacks:
Arabic, Hebrew, Armenian, Georgian go to DejaVu Sans), Noto Sans Symbols,
Symbols 2 and Math. Android's named families and those fallbacks are mapped
onto them; scripts the host has no font for (CJK, Indic, South-East Asian,
historic) and emoji keep Android's fonts, and the sync replaces any of
those with a same-named host font if the host ships one.

Inputs: fonts-lineage-20.xml (the image's /system/etc/fonts.xml) and
android-font-files.txt (the image's /system/fonts and product/fonts).
Outputs: fonts.xml, host-fonts.txt (host files fonts.xml names; all must
exist or the sync keeps Android's fonts), android-fonts.txt (Android files
fonts.xml still names), hidden-fonts.txt (Android files nothing names any
more; the sync hides them).
"""
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))

SANS = [
    (100, 'normal', 'SourceSans3-ExtraLight.ttf'),
    (200, 'normal', 'SourceSans3-ExtraLight.ttf'),
    (300, 'normal', 'SourceSans3-Light.ttf'),
    (400, 'normal', 'SourceSans3-Regular.ttf'),
    (500, 'normal', 'SourceSans3-Semibold.ttf'),   # no Medium: Material asks 500
    (600, 'normal', 'SourceSans3-Semibold.ttf'),
    (700, 'normal', 'SourceSans3-Bold.ttf'),
    (800, 'normal', 'SourceSans3-Bold.ttf'),
    (900, 'normal', 'SourceSans3-Bold.ttf'),
    (300, 'italic', 'SourceSans3-LightIt.ttf'),
    (400, 'italic', 'SourceSans3-It.ttf'),
]
MONO = [(400, 'normal', 'JetBrainsMono-Medium.ttf'), (500, 'normal', 'JetBrainsMono-Medium.ttf')]
NOTO_SANS = [
    (400, 'normal', 'NotoSans-Regular.ttf'), (700, 'normal', 'NotoSans-Bold.ttf'),
    (400, 'italic', 'NotoSans-Italic.ttf'), (700, 'italic', 'NotoSans-BoldItalic.ttf'),
]
DEJAVU = [(400, 'normal', 'DejaVuSans.ttf'), (700, 'normal', 'DejaVuSans-Bold.ttf')]

NAMED = {
    'sans-serif': SANS,
    'sans-serif-condensed': SANS,   # Marathon has no condensed face
    'monospace': MONO,
    'serif-monospace': MONO,
    'casual': SANS,
    'cursive': SANS,
    'sans-serif-smallcaps': SANS,
    'source-sans-pro': SANS,
}
# serif keeps Android's entries: they name NotoSerif-*.ttf, which the host
# ships under the same names, so the host's copies replace them.
HOST_SERIF = ['NotoSerif-Regular.ttf', 'NotoSerif-Bold.ttf',
              'NotoSerif-Italic.ttf', 'NotoSerif-BoldItalic.ttf']
# DejaVu Sans (fontconfig's pick for Arabic, Hebrew, Armenian, Georgian on
# the host) also carries monochrome emoticons, and Minikin's fallback then
# drew U+1F600 from it instead of the colour emoji: those scripts keep
# Android's Noto.
BY_LANG = {}
EXTRA_ALIASES = [('google-sans', 'sans-serif', None), ('google-sans-text', 'sans-serif', None),
                 ('google-sans-medium', 'sans-serif', 500), ('google-sans-bold', 'sans-serif', 700),
                 ('roboto', 'sans-serif', None), ('roboto-medium', 'sans-serif', 500)]


def fonts(entries, indent='        '):
    return '\n'.join('%s<font weight="%d" style="%s">%s</font>' % (indent, w, s, f)
                     for w, s, f in entries)


def family(attrs, entries):
    return '    <family%s>\n%s\n    </family>' % (attrs, fonts(entries))


def main():
    xml = open(os.path.join(HERE, 'fonts-lineage-20.xml')).read()
    fam = re.compile(r'    <family( [^>]*)?>.*?</family>', re.S)

    def replace(m):
        attrs = m.group(1) or ''
        name = re.search(r'name="([^"]+)"', attrs)
        lang = re.search(r'lang="([^"]+)"', attrs)
        body = m.group(0)
        if name and name.group(1) in NAMED:
            return family(' name="%s"' % name.group(1), NAMED[name.group(1)])
        if lang and lang.group(1) in BY_LANG:
            return family(attrs, BY_LANG[lang.group(1)])
        if 'ignore="true"' in attrs and 'NotoColorEmojiLegacy' in body:
            return ''
        if 'NotoSansSymbols-Regular-Subsetted.ttf' in body:
            return family('', NOTO_SANS)
        if 'NotoSansSymbols-Regular-Subsetted2.ttf' in body:
            # Android's symbol fonts were subset to leave emoji to the colour
            # font; the host's full ones go after the emoji families so their
            # monochrome emoticons never win.
            return '\n'.join([family('', [(400, 'normal', 'NotoSansSymbols-Regular.ttf')]),
                              family('', [(400, 'normal', 'NotoSansMath-Regular.ttf')]),
                              family(attrs, [(400, 'normal', 'NotoSansSymbols2-Regular.ttf')])])
        return body

    out = fam.sub(replace, xml)
    aliases = '\n'.join('    <alias name="%s" to="%s"%s />' % (n, t, ' weight="%d"' % w if w else '')
                        for n, t, w in EXTRA_ALIASES)
    out = out.replace('    <alias name="sans-serif-thin"', aliases + '\n    <alias name="sans-serif-thin"', 1)
    open(os.path.join(HERE, 'fonts.xml'), 'w').write(out)

    named = set(re.findall(r'>\s*([\w.-]+\.(?:ttf|otf|ttc))', out))
    host = sorted({f for e in list(NAMED.values()) + list(BY_LANG.values()) + [NOTO_SANS] for _, _, f in e}
                  | set(HOST_SERIF) | {'NotoSansSymbols-Regular.ttf', 'NotoSansSymbols2-Regular.ttf',
                                       'NotoSansMath-Regular.ttf'})
    android = [l.strip() for l in open(os.path.join(HERE, 'android-font-files.txt')) if l.strip()]
    kept = sorted(f for f in android if f in named and f not in host)
    # Only what Android's own fonts.xml (or fonts_customization.xml, emptied
    # by fonts_customization.xml here) named: code may open the rest by path.
    original = set(re.findall(r'>\s*([\w.-]+\.(?:ttf|otf|ttc))', xml))
    custom = {'product/' + f for f in re.findall(r'>\s*([\w.-]+\.(?:ttf|otf|ttc))',
                                                  open(os.path.join(HERE, 'fonts_customization-lineage-20.xml')).read())}
    hidden = sorted(f for f in android if f not in named and (f in original or f in custom))
    for name, items in (('host-fonts.txt', host), ('android-fonts.txt', kept),
                        ('hidden-fonts.txt', hidden)):
        open(os.path.join(HERE, name), 'w').write('\n'.join(items) + '\n')
    assert named <= set(host) | set(android), sorted(named - set(host) - set(android))


if __name__ == '__main__':
    main()
