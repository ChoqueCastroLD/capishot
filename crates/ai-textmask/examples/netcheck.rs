//! Checks the burn network against the ONNX file: writes the logits of a raw RGB tile.
//!
//! netcheck <weights.safetensors> <tile.rgb> <side> <out.f32>

#![allow(clippy::unwrap_used, clippy::expect_used)]

use photocraft_ai_textmask::net::Net;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let side: usize = a[3].parse().unwrap();
    let net = Net::from_safetensors(&std::fs::read(&a[1]).unwrap(), &burn::tensor::Device::flex()).unwrap();
    let rgb = std::fs::read(&a[2]).unwrap();
    let t = std::time::Instant::now();
    let out = pollster(net.logits_async(&rgb, side)).unwrap();
    println!("{} logits in {:.1}s", out.len(), t.elapsed().as_secs_f32());
    let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(&a[4], bytes).unwrap();
}

/// Runs a future that is ready on the CPU backend.
fn pollster<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
