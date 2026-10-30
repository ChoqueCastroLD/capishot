"""Write the cold-function list from `wasm-split --print-profile` output (profile.sh): the names,
without the crates' [hash] disambiguators, of every function the profiled flows never ran.

    python3 cold_list.py <printed profile> <cold-functions.txt.gz>
"""
import gzip
import re
import sys

HASH = re.compile(r'\[[0-9a-f]{16}\]')
lines = open(sys.argv[1], encoding='utf8').read().splitlines()
ran = {HASH.sub('', l[2:]) for l in lines if l.startswith('+ ')}
never = {HASH.sub('', l[2:]) for l in lines if l.startswith('- ')}
# A name shared by a function that ran stays (the list is by name), as do unnamed ones.
cold = sorted(n for n in never if n not in ran and not n.isdigit())
with gzip.GzipFile(sys.argv[2], 'wb', mtime=0) as f:
    f.write(('# Functions the basic page flows never ran (profile.sh): split.py moves them to lazily loaded chunks.\n'
             + '\n'.join(cold) + '\n').encode('utf8'))
print(f'{len(cold)} cold functions ({len(ran)} ran)')
