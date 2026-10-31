#!/usr/bin/env python3
"""Split a web build's WebAssembly module into a first-load module and lazily loaded chunks.

    split.py --wasm <module_bg.wasm> --glue <module.js> --cold <cold-functions.txt> [--chunk-bytes N]

The module must carry its name section (the builds keep it for this step). Functions listed in
cold-functions.txt (names without the crates' [hash] disambiguators, one per line: the functions a
profile of the basic page flows never ran, see profile.sh) move to chunk modules of at most
--chunk-bytes of code each, kept under the 8 MB Chrome allows for a synchronous compile on the page's
thread. Everything else, and every function not in the list (new code), stays in the first-load
module. Exported functions and functions whose type mentions v128 (a JavaScript placeholder can't
forward those) stay too.

The module and its chunks are rewritten in place (<module>_bg.wasm, then <module>_bg.cN.wasm next
to it) without the name section, and the glue (<module>.js) gets the loader (loader.js): a call
into a chunk not loaded yet loads it on the spot (the page's thread fetches it synchronously if the
background fetch hasn't brought it yet; a worker always can), and the page fetches and instantiates
every chunk in the background once the app has started.
"""
import argparse
import gzip
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from wasm_info import info, placeholder_slots, placeholder_table, strip_names  # noqa: E402

FEATURES = [
    '--enable-mutable-globals', '--enable-nontrapping-float-to-int', '--enable-bulk-memory', '--enable-sign-ext',
    '--enable-reference-types', '--enable-multivalue', '--enable-bulk-memory-opt',
]
# The crates' disambiguators: 16 hex digits, fewer when the hash starts with zeros.
HASH = re.compile(r'\[[0-9a-f]{1,16}\]')
# LLVM's suffix for a duplicated symbol (`read_sample.3510`), which differs between builds.
SUFFIX = re.compile(r'\.\d+$')


def norm(name):
    return SUFFIX.sub('', HASH.sub('', name))


def crate_of(name):
    m = re.search(r'([A-Za-z_][A-Za-z0-9_]*)\[[0-9a-f]+\]::', name)
    return m.group(1) if m else '~'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--wasm', required=True)
    ap.add_argument('--glue', required=True)
    ap.add_argument('--cold', required=True)
    ap.add_argument('--wasm-split', default=os.environ.get('WASM_SPLIT', 'wasm-split'))
    ap.add_argument('--hot', help='the commands the profiled flows ran (hot-commands*.txt): they run without waiting for the chunks')
    ap.add_argument('--chunk-bytes', type=float, default=5.5e6)
    ap.add_argument('--threads', action='store_true', help='the module uses threads and SIMD')
    ap.add_argument('--strip-only', action='store_true', help="don't split; only drop the name section")
    a = ap.parse_args()
    if a.strip_only:
        strip_names(a.wasm)
        return

    glue = open(a.glue, encoding='utf8').read()
    get = re.search(r'function __wbg_get_imports\(([^)]*)\) \{', glue)
    if not get or 'wasm = instance.exports;' not in glue:
        sys.exit(f'{a.glue}: not the wasm-bindgen glue this step knows')
    opener = gzip.open if a.cold.endswith('.gz') else open
    cold = {norm(line.rstrip('\n')) for line in opener(a.cold, 'rt', encoding='utf8') if line.strip() and not line.startswith('#')}
    funcs = info(a.wasm)
    named = [f for f in funcs if not f[4].startswith('$f')]
    if len(named) < len(funcs) // 2:
        sys.exit(f'{a.wasm}: no name section; build it with names (wasm-opt --debuginfo) for the split')
    moving = [f for f in funcs if norm(f[4]) in cold and not f[2] and not f[3]]
    # Chunks: the moving functions grouped by crate, then name, packed up to the code budget.
    moving.sort(key=lambda f: (crate_of(f[4]), f[4]))
    chunks, size = [[]], 0
    for f in moving:
        if size + f[1] > a.chunk_bytes and chunks[-1]:
            chunks.append([])
            size = 0
        chunks[-1].append(f[4])
        size += f[1]
    chunks = [c for c in chunks if c]
    if not chunks:
        print('split: nothing to move')
        strip_names(a.wasm)
        return
    work = a.wasm + '.split'
    os.makedirs(work, exist_ok=True)
    manifest = os.path.join(work, 'manifest.txt')
    with open(manifest, 'w', encoding='utf8') as m:
        for i, c in enumerate(chunks):
            m.write(f'c{i}\n' + '\n'.join(c) + '\n\n')
    features = FEATURES + (['--enable-threads', '--enable-simd'] if a.threads else [])
    primary = os.path.join(work, 'primary.wasm')
    subprocess.run([a.wasm_split, '--multi-split', *features, '--manifest', manifest, '--out-prefix', work + '/',
                    '-o', primary, '--strip-debug', a.wasm], check=True)
    # The table the chunks fill.
    added = [placeholder_table(primary)]
    # Placeholders anywhere else (another function table) would stay after their chunk loads.
    stray = [s for v in placeholder_slots(primary).values() for s in v if s[0] != added[0]]
    if stray:
        sys.exit(f'{primary}: {len(stray)} placeholders outside the split table, e.g. {stray[:3]}')
    base = a.wasm[:-len('.wasm')]
    names = {}
    for i in range(len(chunks)):
        dst = f'{base}.c{i}.wasm'
        os.replace(os.path.join(work, f'c{i}.wasm'), dst)
        size = os.path.getsize(dst)
        if size >= 8 * 1024 * 1024:
            sys.exit(f'{dst}: {size} bytes, over the 8 MiB a synchronous compile allows; lower --chunk-bytes')
        names[f'c{i}'] = os.path.basename(dst)
    os.replace(primary, a.wasm)
    os.remove(manifest)
    os.rmdir(work)
    # The glue: the loader in the module's scope, its imports added, and told when an instance is up.
    hot = sorted(line.strip() for line in open(a.hot, encoding='utf8') if line.strip() and not line.startswith('#')) if a.hot else []
    loader = open(os.path.join(HERE, 'loader.js'), encoding='utf8').read().replace('__SPLIT_CHUNKS__', json.dumps(names)).replace('__SPLIT_TABLE__', json.dumps(added[0])).replace('__SPLIT_HOT__', json.dumps(hot))
    params = get.group(1)
    glue = glue.replace(get.group(0), f'function __wbg_get_imports_unsplit({params}) {{', 1)
    glue = glue.replace('wasm = instance.exports;', 'wasm = instance.exports;\n    __splitInstanceReady();', 1)
    glue += (f'\n{loader}\nfunction __wbg_get_imports({params}) {{\n    const imports = __wbg_get_imports_unsplit({params});\n'
             '    __splitAddImports(imports);\n    return imports;\n}\n')
    open(a.glue, 'w', encoding='utf8').write(glue)
    total = sum(f[1] for f in moving)
    print(f'split: {len(moving)} functions ({total / 1e6:.1f} MB of code) into {len(chunks)} chunks; '
          f'first-load module {os.path.getsize(a.wasm)} bytes')


if __name__ == '__main__':
    main()
