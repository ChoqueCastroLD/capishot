"""Recompute every integrity="sha384-..." in an HTML file from the file its href/src names
(relative to the HTML file), after a build step rewrote some of them."""
import base64
import hashlib
import os
import re
import sys

path = sys.argv[1]
base = os.path.dirname(path)
html = open(path, encoding='utf8').read()


def fix(m):
    tag = m.group(0)
    url = re.search(r'\s(?:href|src)="([^"]+)"', tag)
    if not url:
        return tag
    f = os.path.join(base, url.group(1).split('?')[0])
    digest = base64.b64encode(hashlib.sha384(open(f, 'rb').read()).digest()).decode()
    return re.sub(r'integrity="sha384-[^"]*"', f'integrity="sha384-{digest}"', tag)


out = re.sub(r'<(?:link|script)\b[^>]*\bintegrity="sha384-[^"]*"[^>]*>', fix, html)
open(path, 'w', encoding='utf8').write(out)
