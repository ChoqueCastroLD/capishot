"""Independent eager PyTorch reference for tests/tiny_compact.rs (SRVGGNetCompact as Real-ESRGAN
defines it: conv + PReLU stack, pixel_shuffle, plus the nearest-upsampled input; x2 as the x4
result box-downsampled). Writes tiny_compact_x4.in and tiny_compact_x2.in next to this file.

    cargo test -p photocraft-ai --test tiny_compact -- --ignored dump
    python -I crates/ai/tests/tiny_compact_reference.py   # needs torch + safetensors
"""
import os, tempfile
import numpy as np, torch, torch.nn.functional as F
from safetensors.torch import load_file

d = os.path.join(tempfile.gettempdir(), 'compact-tiny')
W_ = load_file(os.path.join(d, 'tiny.safetensors'))
Wd, Hd = 7, 5
x = torch.from_numpy(np.fromfile(os.path.join(d, 'in.f32'), '<f4').reshape(1, 3, Hd, Wd))
with torch.no_grad():
    t = x
    for i in range(4):
        t = F.conv2d(t, W_[f'body.{2 * i}.weight'], W_[f'body.{2 * i}.bias'], padding=1)
        if i < 3:
            t = F.prelu(t, W_[f'body.{2 * i + 1}.weight'])
    x4 = F.pixel_shuffle(t, 4) + F.interpolate(x, scale_factor=4, mode='nearest')
    x2 = F.avg_pool2d(x4, 2)
here = os.path.dirname(os.path.abspath(__file__))
for name, out in (('x4', x4), ('x2', x2)):
    flat = out.reshape(-1)
    picks = list(range(0, flat.numel(), max(1, flat.numel() // 23)))
    with open(os.path.join(here, f'tiny_compact_{name}.in'), 'w') as f:
        f.write('[\n' + ''.join(f'    ({i}, {flat[i].item():.7f}),\n' for i in picks) + ']\n')
print('x4 mean', x4.mean().item(), 'std', x4.std().item())
