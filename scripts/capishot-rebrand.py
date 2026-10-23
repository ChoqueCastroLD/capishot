#!/usr/bin/env python3
"""Re-applies Capishot's branding on top of PhotoCraft sources.

Capishot tracks storytold/photocraft. After merging upstream, run this from the repository root;
it is idempotent. Crate names, file names and ids stay `photocraft-*` so merges stay small; the
user-facing product name, binaries, app id and settings folders become Capishot's. Copyright
lines that credit PhotoCraft are kept, and so are the names of the built-in colour profiles.
"""
import os
import re
import sys

KEEP = ['Based on PhotoCraft', 'gamma 1.732, PhotoCraft', '-compatible (PhotoCraft)', 'PhotoCraftCo', 'PhotoCraft contributors', 'PhotoCraft authors', 'storytold/photocraft']
EXTS = ('.rs', '.tsv', '.html', '.toml', '.css', '.js', '.json', '.md')
SKIP_DIRS = ('crates/cms',)
# Byte-exact test fixtures whose lengths the tests depend on.
SKIP_FILES = ('crates/codecs/tests/orientation.rs',)


def sub_file(path, pairs, regex=False):
    try:
        s = open(path, encoding='utf-8').read()
    except FileNotFoundError:
        return False
    o = s
    for a, b in pairs:
        s = re.sub(a, b, s) if regex else s.replace(a, b)
    if s != o:
        open(path, 'w', encoding='utf-8').write(s)
    return s != o


def product_name():
    changed = 0
    for root in ('apps', 'crates', 'xtask', 'packaging/web-threads'):
        for dp, dn, fn in os.walk(root):
            dn[:] = [d for d in dn if d != 'target']
            if dp.startswith(SKIP_DIRS):
                continue
            for f in fn:
                if not f.endswith(EXTS):
                    continue
                p = os.path.join(dp, f)
                if p in SKIP_FILES:
                    continue
                s = open(p, encoding='utf-8', errors='surrogateescape').read()
                o = s
                for i, k in enumerate(KEEP):
                    s = s.replace(k, f'\x00K{i}\x00')
                if f.endswith('.tsv'):
                    # Translations of "Based on PhotoCraft" name the upstream project: keep them.
                    s = '\n'.join(l if '\x00K0\x00' in l else l.replace('PhotoCraft', 'Capishot') for l in s.split('\n'))
                else:
                    s = s.replace('PhotoCraft', 'Capishot')
                for i, k in enumerate(KEEP):
                    s = s.replace(f'\x00K{i}\x00', k)
                if s != o:
                    open(p, 'w', encoding='utf-8', errors='surrogateescape').write(s)
                    changed += 1
    return changed


def binaries_and_dirs():
    sub_file('apps/photocraft/Cargo.toml', [
        ('default-run = "photocraft"', 'default-run = "capishot"'),
        ('Photocraft: an open-source, native image editor"', 'Capishot: an open-source, native image editor for scanlation"'),
    ])
    p = 'apps/photocraft/Cargo.toml'
    s = open(p).read()
    if '[[bin]]' not in s:
        s = s.replace('[lints]\nworkspace = true\n', '[[bin]]\nname = "capishot"\npath = "src/main.rs"\n\n[lints]\nworkspace = true\n', 1)
        open(p, 'w').write(s)
    sub_file('apps/photocraft-cli/Cargo.toml', [('Headless Photocraft', 'Headless Capishot')])
    sub_file('apps/photocraft-cli/Cargo.toml', [(r'(\[\[bin\]\]\nname = )"photocraft-cli"', r'\1"capishot-cli"')], regex=True)
    sub_file('apps/photocraft-web/Cargo.toml', [('Photocraft in the browser', 'Capishot in the browser')])
    sub_file('apps/photocraft/build.rs', [
        ('photocraft.exe', 'capishot.exe'),
        ('.set("InternalName", "photocraft")', '.set("InternalName", "capishot")'),
        ('Copyright (c) the PhotoCraft authors.', 'Copyright (c) the Capishot and PhotoCraft authors.'),
    ])
    sub_file('apps/photocraft/src/app_dirs.rs', [
        ('Application Support/Photocraft', 'Application Support/Capishot'),
        ('.join("Photocraft")', '.join("Capishot")'),
        ('c.join("photocraft")', 'c.join("capishot")'),
        ('root.join("home/.config/photocraft")', 'root.join("home/.config/capishot")'),
    ])
    sub_file('apps/photocraft/src/main.rs', [('const APP_ID: &str = "ai.storyteller.photocraft";', 'const APP_ID: &str = "io.github.choquecastrold.capishot";')])
    for t in sorted(os.listdir('apps/photocraft/tests')):
        if t.endswith('.rs'):
            sub_file(os.path.join('apps/photocraft/tests', t), [('CARGO_BIN_EXE_photocraft")', 'CARGO_BIN_EXE_capishot")')])
    # The CLI binary name in usage text, docs comments and tests (not the package name).
    for p in ['apps/photocraft-cli/src/lib.rs', 'apps/photocraft-cli/tests/cli.rs', 'apps/photocraft-cli/tests/orientation.rs',
              'crates/automation/src/lib.rs', 'crates/automation/src/rpc.rs', 'crates/automation/src/server.rs',
              'crates/engine/src/lib.rs', 'apps/photocraft/src/main.rs', 'crates/engine/src/automate_cmds.rs']:
        sub_file(p, [(r'photocraft-cli\b', 'capishot-cli'), (r'headless Photocraft', 'headless Capishot')], regex=True)


if __name__ == '__main__':
    if not os.path.isfile('Cargo.toml') or not os.path.isdir('crates'):
        sys.exit('run from the repository root')
    n = product_name()
    binaries_and_dirs()
    print(f'rebranded {n} files')
