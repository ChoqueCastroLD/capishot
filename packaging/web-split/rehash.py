"""Give a Trunk build's glue and module (and the split's chunks) a file-name hash of their content
after the split rewrote them. Trunk's hash doesn't change with the split, so without this a split
and an unsplit build of the same source would share names, and a browser holding one build's glue
and the other's module (both cached as immutable) would fail to link them.

    python3 rehash.py <dist dir>
"""
import glob
import hashlib
import os
import re
import sys

d = sys.argv[1]
wasm = glob.glob(os.path.join(d, '*_bg.wasm'))[0]
m = re.fullmatch(r'(.+)-([0-9a-f]{16})_bg\.wasm', os.path.basename(wasm))
if not m:
    sys.exit(f'{wasm}: no content hash in the name')
stem, old = m.groups()
files = sorted(glob.glob(os.path.join(d, f'{stem}-{old}*')))
h = hashlib.sha256()
for f in files:
    h.update(open(f, 'rb').read())
new = h.hexdigest()[:16]
a, b = f'{stem}-{old}', f'{stem}-{new}'
for f in files:
    dst = os.path.join(d, os.path.basename(f).replace(a, b, 1))
    if f.endswith('.js'):
        # The loader's chunk names.
        s = open(f, encoding='utf8').read().replace(a, b)
        open(f, 'w', encoding='utf8').write(s)
    os.replace(f, dst)
for page in glob.glob(os.path.join(d, '*.html')):
    s = open(page, encoding='utf8').read()
    open(page, 'w', encoding='utf8').write(s.replace(a, b))
print(f'rehash: {a} -> {b}')
