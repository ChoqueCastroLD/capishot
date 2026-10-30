# egui-wgpu (vendored)

egui-wgpu 0.37.0 as in `storytold/egui` rev `6d518fc799681571963842f95b55a6b68431b15a`, except
`src/renderer.rs` and `src/egui.wgsl`, which are egui-wgpu 0.36.2's.

0.37 added a uniform buffer to every texture's bind group (the texture's wrap mode and filter,
read by the shader only when `predictable_texture_filtering` is on, which Capishot never turns
on). Binding it on every draw call cost the WebGL2 backend a lot on a software or phone-class
GPU: with a phone emulated in Chromium (412 × 915, SwiftShader, CPU 4× slower), a brush stroke
drew 43 frames instead of 68 and the GPU process spent three times as long per frame. With
0.36's renderer the frame rate is back to the 0.36 build's.

Drop this copy (and its `[patch.crates-io]` line in the workspace `Cargo.toml`) once egui-wgpu
skips that binding when predictable filtering is off. Licence: MIT OR Apache-2.0 (the files here).
