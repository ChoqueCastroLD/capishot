"""Independent PyTorch reference for tests/tiny_model.rs (eager convs + torch.fft, not the DFT
matrices of src/dft.rs). Writes tiny_expected.in and tiny_expected_mean.in next to this file.

    cargo test -p photocraft-ai-lama --features flex --test tiny_model -- --ignored dump
    python -I crates/ai-lama/tests/tiny_reference.py   # needs torch + safetensors
"""
import os, tempfile
import numpy as np, torch, torch.nn.functional as F
from safetensors.torch import load_file

d = os.path.join(tempfile.gettempdir(), 'lama-tiny')
W_ = load_file(os.path.join(d, 'tiny.safetensors'))
Wd, Hd = 72, 64
rgb = torch.from_numpy(np.fromfile(os.path.join(d, 'rgb.f32'), '<f4').reshape(Hd, Wd, 3)).permute(2, 0, 1)[None]
mask = torch.from_numpy(np.fromfile(os.path.join(d, 'mask.f32'), '<f4').reshape(1, 1, Hd, Wd))
mask = (mask > 0).float()

def conv(x, n, stride=1, pad=1):
    if pad:
        x = F.pad(x, [pad] * 4, mode='reflect')
    return F.conv2d(x, W_[n + '.w'], W_.get(n + '.b'), stride=stride)

def fourier(x, q):
    c = x.shape[1]
    f = torch.fft.rfftn(x, dim=(-2, -1), norm='ortho')
    y = torch.cat([f.real, f.imag], 1)  # the file's blocked [re..., im...] channel layout
    y = F.relu(F.conv2d(y, W_[q + '.fu.w'], W_[q + '.fu.b']))
    return torch.fft.irfftn(torch.complex(y[:, :c], y[:, c:]), s=x.shape[-2:], dim=(-2, -1), norm='ortho')

def ffc(xl, xg, q):
    ol = conv(xl, q + '.l2l') + conv(xg, q + '.g2l')
    z = F.relu(F.conv2d(xg, W_[q + '.st1.w'], W_[q + '.st1.b']))
    og = conv(xl, q + '.l2g') + F.conv2d(z + fourier(z, q), W_[q + '.st2.w'])
    return F.relu(ol), F.relu(og)

x = torch.cat([rgb * (1 - mask), mask], 1)
with torch.no_grad():
    x = F.relu(conv(x, 'in', pad=3))
    x = F.relu(conv(x, 'down1', 2)); x = F.relu(conv(x, 'down2', 2))
    xl = F.relu(conv(x, 'down3.l', 2)); xg = F.relu(conv(x, 'down3.g', 2))
    for b in range(2):
        al, ag = ffc(xl, xg, f'blocks.{b}.conv1'); al, ag = ffc(al, ag, f'blocks.{b}.conv2')
        xl, xg = xl + al, xg + ag
    x = torch.cat([xl, xg], 1)
    for u in ('up1', 'up2', 'up3'):
        x = F.relu(F.conv_transpose2d(x, W_[u + '.w'], W_[u + '.b'], stride=2, padding=1, output_padding=1))
    out = torch.sigmoid(conv(x, 'out', pad=3))[0].permute(1, 2, 0).reshape(-1, 3)
here = os.path.dirname(os.path.abspath(__file__))
picks = [(i, i % 3) for i in range(0, Wd * Hd, 397)] + [(30 * Wd + 30, 1), (25 * Wd + 40, 2)]
with open(os.path.join(here, 'tiny_expected.in'), 'w') as f:
    f.write('[\n' + ''.join(f'    ({i}, {c}, {out[i, c].item():.7f}),\n' for i, c in picks) + ']\n')
with open(os.path.join(here, 'tiny_expected_mean.in'), 'w') as f:
    f.write(f'{out.mean().item():.7f}\n')
print('mean', out.mean().item(), 'std', out.std().item())
