#!/usr/bin/env python3
"""manga-text-mask.onnx -> safetensors for the browser (crates/ai-textmask/src/net.rs).

The convolutions in graph order as c00.w, c00.b, ... in fp16, with the one batch norm folded into
the convolution before it. Needs onnx, numpy and safetensors.

    convert-textmask.py manga-text-mask.onnx manga-text-mask-f16.safetensors
"""
import sys

import numpy as np
import onnx
from onnx import numpy_helper
from safetensors.numpy import save_file


def main(src, dst):
    model = onnx.load(src)
    init = {i.name: numpy_helper.to_array(i) for i in model.graph.initializer}
    nodes = model.graph.node
    convs = [n for n in nodes if n.op_type == "Conv"]
    bn = next(n for n in nodes if n.op_type == "BatchNormalization")
    out = {}
    for k, conv in enumerate(convs):
        w = init[conv.input[1]].astype(np.float64)
        b = init[conv.input[2]].astype(np.float64) if len(conv.input) > 2 else None
        if conv.output[0] == bn.input[0]:
            scale, bias, mean, var = (init[bn.input[i]].astype(np.float64) for i in range(1, 5))
            eps = next(a.f for a in bn.attribute if a.name == "epsilon")
            g = scale / np.sqrt(var + eps)
            w = w * g.reshape(-1, 1, 1, 1)
            b = bias - mean * g
        out[f"c{k:02}.w"] = w.astype(np.float16)
        if b is not None:
            out[f"c{k:02}.b"] = b.astype(np.float16)
    save_file(out, dst, metadata={"arch": "text-mask-unet", "format": "pt"})
    print(f"{len(convs)} convolutions -> {dst}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
