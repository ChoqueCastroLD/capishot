"""Write the cold-function list from `wasm-split --print-profile` output (profile.sh): the names,
without the crates' [hash] disambiguators, of every function the profiled flows never ran.

    python3 cold_list.py <printed profile> <cold-functions.txt.gz>
"""
import gzip
import re
import sys

# As split.py's `norm`: the crates' disambiguators (16 hex digits or fewer) and LLVM's numeric
# suffix of duplicated symbols go.
HASH = re.compile(r'\[[0-9a-f]{1,16}\]')
SUFFIX = re.compile(r'\.\d+$')
lines = open(sys.argv[1], encoding='utf8').read().splitlines()
norm = lambda n: SUFFIX.sub('', HASH.sub('', n))
ran = {norm(l[2:]) for l in lines if l.startswith('+ ')}
never = {norm(l[2:]) for l in lines if l.startswith('- ')}
# A name shared by a function that ran stays (the list is by name), as do unnamed ones.
cold = sorted(n for n in never if n not in ran and not n.isdigit())
with gzip.GzipFile(sys.argv[2], 'wb', mtime=0) as f:
    f.write(('# Functions the basic page flows never ran (profile.sh): split.py moves them to lazily loaded chunks.\n'
             + '\n'.join(cold) + '\n').encode('utf8'))
print(f'{len(cold)} cold functions ({len(ran)} ran)')
