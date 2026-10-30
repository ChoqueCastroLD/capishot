"""Read what the split needs from a WebAssembly module: every defined function's name (from the
name section), body size, whether its type mentions v128 (a JS placeholder can't forward
those), and which functions are exported.

    python3 wasm_info.py module.wasm   prints one line per defined function: index size v128 exported name
"""
import sys


def leb(b, i):
    r = s = 0
    while True:
        x = b[i]
        i += 1
        r |= (x & 0x7F) << s
        s += 7
        if x < 0x80:
            return r, i


def sleb(b, i):
    r = s = 0
    while True:
        x = b[i]
        i += 1
        r |= (x & 0x7F) << s
        s += 7
        if x < 0x80:
            if x & 0x40:
                r -= 1 << s
            return r, i


V128 = 0x7B
# Exported table names by module path, filled by `info`.
TABLE_EXPORTS = {}


def read_valtype(b, i):
    t = b[i]
    i += 1
    if t in (0x63, 0x64):  # (ref null ht) / (ref ht)
        _, i = sleb(b, i)
    return t, i


def table_exports(path):
    """The names of the tables `path` exports."""
    TABLE_EXPORTS.pop(path, None)
    info(path)
    return TABLE_EXPORTS.get(path, [])


def info(path):
    TABLE_EXPORTS[path] = []
    b = open(path, 'rb').read()
    assert b[:4] == b'\0asm', 'not a wasm module'
    i = 8
    types = []  # per type: mentions v128
    n_imported_funcs = 0
    func_types = []
    bodies = []
    exported = set()
    names = {}
    while i < len(b):
        sid = b[i]
        i += 1
        size, i = leb(b, i)
        end = i + size
        j = i
        if sid == 1:  # types
            n, j = leb(b, j)
            for _ in range(n):
                form = b[j]
                j += 1
                if form == 0x4E:  # rec group
                    raise SystemExit('rec groups not supported')
                if form == 0x50 or form == 0x4F:  # sub types
                    m, j = leb(b, j)
                    for _ in range(m):
                        _, j = leb(b, j)
                    form = b[j]
                    j += 1
                assert form == 0x60, f'type form {form:#x}'
                has = False
                for _ in range(2):
                    m, j = leb(b, j)
                    for _ in range(m):
                        t, j = read_valtype(b, j)
                        has |= t == V128
                types.append(has)
        elif sid == 2:  # imports
            n, j = leb(b, j)
            for _ in range(n):
                for _ in range(2):
                    m, j = leb(b, j)
                    j += m
                kind = b[j]
                j += 1
                if kind == 0:
                    _, j = leb(b, j)
                    n_imported_funcs += 1
                elif kind == 1:  # table
                    _, j = read_valtype(b, j)
                    flags = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                elif kind == 2:  # memory
                    flags = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                elif kind == 3:  # global
                    _, j = read_valtype(b, j)
                    j += 1
                elif kind == 4:  # tag
                    j += 1
                    _, j = leb(b, j)
                else:
                    raise SystemExit(f'import kind {kind}')
        elif sid == 3:
            n, j = leb(b, j)
            for _ in range(n):
                t, j = leb(b, j)
                func_types.append(t)
        elif sid == 7:  # exports
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                ename = b[j:j + m].decode('utf8', 'replace')
                j += m
                kind = b[j]
                j += 1
                idx, j = leb(b, j)
                if kind == 0:
                    exported.add(idx)
                elif kind == 1:
                    TABLE_EXPORTS.setdefault(path, []).append(ename)
        elif sid == 10:
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                bodies.append(m)
                j += m
        elif sid == 0:
            m, j = leb(b, j)
            nm = b[j:j + m].decode('utf8', 'replace')
            j += m
            if nm == 'name':
                while j < end:
                    sub = b[j]
                    j += 1
                    ssz, j = leb(b, j)
                    send = j + ssz
                    if sub == 1:
                        cnt, k = leb(b, j)
                        for _ in range(cnt):
                            idx, k = leb(b, k)
                            ln, k = leb(b, k)
                            names[idx] = b[k:k + ln].decode('utf8', 'replace')
                            k += ln
                    j = send
        i = end
    out = []
    for n, (t, size) in enumerate(zip(func_types, bodies)):
        idx = n_imported_funcs + n
        out.append((idx, size, types[t], idx in exported, names.get(idx, f'$f{idx}')))
    return out


if __name__ == '__main__':
    for idx, size, v128, exp, name in info(sys.argv[1]):
        print(idx, size, int(v128), int(exp), name, sep='\t')


def placeholder_table(path):
    """The name of the exported table the split's placeholders live in: the funcref table whose
    size is the highest placeholder index plus one."""
    b = open(path, 'rb').read()
    i = 8
    imported_tables = 0
    sizes = []
    names = {}
    top = -1
    while i < len(b):
        sid = b[i]
        i += 1
        size, i = leb(b, i)
        end = i + size
        j = i
        if sid == 2:
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                mod = b[j:j + m].decode()
                j += m
                m, j = leb(b, j)
                field = b[j:j + m].decode()
                j += m
                kind = b[j]
                j += 1
                if kind == 0:
                    _, j = leb(b, j)
                    if mod.startswith('placeholder'):
                        top = max(top, int(field))
                elif kind == 1:
                    _, j = read_valtype(b, j)
                    flags = b[j]
                    j += 1
                    mn, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                    imported_tables += 1
                    sizes.append(mn)
                elif kind == 2:
                    flags = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                elif kind == 3:
                    _, j = read_valtype(b, j)
                    j += 1
                elif kind == 4:
                    j += 1
                    _, j = leb(b, j)
        elif sid == 4:
            n, j = leb(b, j)
            for _ in range(n):
                _, j = read_valtype(b, j)
                flags = b[j]
                j += 1
                mn, j = leb(b, j)
                if flags & 1:
                    _, j = leb(b, j)
                sizes.append(mn)
        elif sid == 7:
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                ename = b[j:j + m].decode()
                j += m
                kind = b[j]
                j += 1
                idx, j = leb(b, j)
                if kind == 1:
                    names[idx] = ename
        i = end
    for idx, mn in enumerate(sizes):
        if mn == top + 1 and idx in names:
            return names[idx]
    raise SystemExit(f'{path}: no exported table of {top + 1} entries for the placeholders')


def placeholder_slots(path):
    """Where the module's active element segments put placeholder imports in exported tables other
    than the placeholders' own: {chunk: [[table export, slot, placeholder index], ...]}. A table
    returns its own wrapper for an imported function, so the loader can't recognize these slots by
    looking at them and fixes them from this list."""
    b = open(path, 'rb').read()
    i = 8
    imports = []  # per imported function: (module, field)
    tnames = {}
    segs = []
    while i < len(b):
        sid = b[i]
        i += 1
        size, i = leb(b, i)
        end = i + size
        j = i
        if sid == 2:
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                mod = b[j:j + m].decode()
                j += m
                m, j = leb(b, j)
                field = b[j:j + m].decode()
                j += m
                kind = b[j]
                j += 1
                if kind == 0:
                    _, j = leb(b, j)
                    imports.append((mod, field))
                elif kind == 1:
                    _, j = read_valtype(b, j)
                    flags = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                elif kind == 2:
                    flags = b[j]
                    j += 1
                    _, j = leb(b, j)
                    if flags & 1:
                        _, j = leb(b, j)
                elif kind == 3:
                    _, j = read_valtype(b, j)
                    j += 1
                elif kind == 4:
                    j += 1
                    _, j = leb(b, j)
        elif sid == 7:
            n, j = leb(b, j)
            for _ in range(n):
                m, j = leb(b, j)
                ename = b[j:j + m].decode()
                j += m
                kind = b[j]
                j += 1
                idx, j = leb(b, j)
                if kind == 1:
                    tnames[idx] = ename
        elif sid == 9:
            n, j = leb(b, j)
            for _ in range(n):
                flags, j = leb(b, j)
                table = 0
                if flags & 1:  # passive or declarative: skip
                    if flags & 4:
                        _, j = read_valtype(b, j)
                    else:
                        j += 1
                    m, j = leb(b, j)
                    for _ in range(m):
                        if flags & 4:
                            while b[j] != 0x0B:
                                j += 1
                            j += 1
                        else:
                            _, j = leb(b, j)
                    continue
                if flags & 2:
                    table, j = leb(b, j)
                assert b[j] == 0x41, 'element offset is not an i32.const'
                off, j = sleb(b, j + 1)
                assert b[j] == 0x0B
                j += 1
                if flags & 2:
                    j += 1  # elemkind or reftype
                m, j = leb(b, j)
                funcs = []
                for _ in range(m):
                    if flags & 4:
                        if b[j] == 0xD2:  # ref.func
                            f, j = leb(b, j + 1)
                        else:  # ref.null
                            f = None
                            j += 2
                        assert b[j] == 0x0B
                        j += 1
                    else:
                        f, j = leb(b, j)
                    funcs.append(f)
                segs.append((table, off, funcs))
        i = end
    out = {}
    for table, off, funcs in segs:
        name = tnames.get(table)
        for k, f in enumerate(funcs):
            if f is None or f >= len(imports):
                continue
            mod, field = imports[f]
            if mod.startswith('placeholder.'):
                if name is None:
                    raise SystemExit(f'{path}: a placeholder sits in table {table}, which is not exported')
                out.setdefault(mod[len('placeholder.'):], []).append([name, off + k, int(field)])
    return out


def strip_names(path):
    """Drop the module's name section in place (what the split does when it doesn't run)."""
    b = open(path, 'rb').read()
    out = bytearray(b[:8])
    i = 8
    while i < len(b):
        start = i
        sid = b[i]
        size, j = leb(b, i + 1)
        end = j + size
        keep = True
        if sid == 0:
            m, k = leb(b, j)
            keep = b[k:k + m] != b'name'
        if keep:
            out += b[start:end]
        i = end
    open(path, 'wb').write(out)
