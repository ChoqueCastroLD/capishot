"""Independent eager PyTorch reference for tests/tiny_migan.rs (MI-GAN as upstream's
migan_inference.py defines it: depthwise-separable convs, [1,3,3,1] FIR down/upsampling, lrelu
0.2 * sqrt(2) clamped to 256, fixed noise, skip RGB). Writes tiny_migan_expected.in and
tiny_migan_expected_mean.in next to this file.

    cargo test -p photocraft-ai-lama --features flex --test tiny_migan -- --ignored dump
    python -I crates/ai-lama/tests/tiny_migan_reference.py   # needs torch + safetensors
"""
import os, math, tempfile
import numpy as np, torch, torch.nn.functional as F
from safetensors.torch import load_file

d = os.path.join(tempfile.gettempdir(), 'migan-tiny')
W_ = load_file(os.path.join(d, 'tiny.safetensors'))
R = 32
rgb = torch.from_numpy(np.fromfile(os.path.join(d, 'rgb.f32'), '<f4').reshape(R, R, 3)).permute(2, 0, 1)[None]
mask = torch.from_numpy(np.fromfile(os.path.join(d, 'mask.f32'), '<f4').reshape(1, 1, R, R))
known = (mask <= 0).float()

def act(x):
    return (F.leaky_relu(x, 0.2) * math.sqrt(2)).clamp(-256, 256)

f1 = torch.tensor([1., 3., 3., 1.])
fir = torch.outer(f1, f1) / 64

def down(x):
    c = x.shape[1]
    return F.conv2d(x, fir.expand(c, 1, 4, 4), stride=2, padding=1, groups=c)

def up(x):
    c = x.shape[1]
    n, _, h, w = x.shape
    z = torch.zeros(n, c, 2 * h, 2 * w)
    z[:, :, ::2, ::2] = x
    z = F.pad(z, (2, 1, 2, 1))
    return F.conv2d(z, (fir * 4).expand(c, 1, 4, 4), groups=c)

def sep(x, p, downs=False, ups=False, noise=False):
    c = x.shape[1]
    x = act(F.conv2d(x, W_[p + '.conv1.weight'], W_[p + '.conv1.bias'], padding=1, groups=c))
    if downs:
        x = down(x)
    x = F.conv2d(x, W_[p + '.conv2.weight'])
    if ups:
        x = up(x)
    if noise:
        x = x + W_[p + '.noise_const'] * W_[p + '.noise_strength']
    return act(x)

img = torch.cat([known - 0.5, (rgb * 2 - 1) * known], 1)
with torch.no_grad():
    feats = {}
    x = act(F.conv2d(img, W_[f'encoder.b{R}.fromrgb.weight'], W_[f'encoder.b{R}.fromrgb.bias']))
    r = R
    while r >= 8:
        feats[r] = sep(x, f'encoder.b{r}.conv1')
        x = sep(feats[r], f'encoder.b{r}.conv2', downs=True)
        r //= 2
    feats[4] = sep(x, 'encoder.b4.conv1')
    x = sep(feats[4], 'encoder.b4.conv2')
    x = sep(sep(x, 'synthesis.b4.conv1') + feats[4], 'synthesis.b4.conv2')
    out = F.conv2d(x, W_['synthesis.b4.torgb.weight'], W_['synthesis.b4.torgb.bias'])
    r = 8
    while r <= R:
        p = f'synthesis.b{r}'
        x = sep(sep(x, p + '.conv1', ups=True, noise=True) + feats[r], p + '.conv2', noise=True)
        out = up(out) + F.conv2d(x, W_[p + '.torgb.weight'], W_[p + '.torgb.bias'])
        r *= 2
    out = (out * 0.5 + 0.5).clamp(0, 1)[0].permute(1, 2, 0).reshape(-1, 3)
here = os.path.dirname(os.path.abspath(__file__))
picks = [(i, i % 3) for i in range(0, R * R, 61)] + [(15 * R + 12, 1), (20 * R + 18, 2)]
with open(os.path.join(here, 'tiny_migan_expected.in'), 'w') as f:
    f.write('[\n' + ''.join(f'    ({i}, {c}, {out[i, c].item():.7f}),\n' for i, c in picks) + ']\n')
with open(os.path.join(here, 'tiny_migan_expected_mean.in'), 'w') as f:
    f.write(f'{out.mean().item():.7f}\n')
print('mean', out.mean().item(), 'std', out.std().item())
