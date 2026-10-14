//! What the browser's clipboard hands over, read the way Capishot pastes it (`clipboard.rs` does
//! the reading): which of a clipboard item's types is the image, that image as RGBA8 pixels, the
//! inline image of copied HTML, and an image Capishot copied coming back.

use base64::Engine as _;

/// Image types a clipboard may hold, best first: PNG (screenshots, browsers' Copy Image, what
/// Capishot writes) is lossless and keeps transparency; SVG is last as it's rasterized at its own
/// size.
const IMAGE_TYPES: [&str; 9] = ["image/png", "image/webp", "image/avif", "image/jpeg", "image/gif", "image/bmp", "image/tiff", "image/x-icon", "image/svg+xml"];

/// The largest clipboard image pasted: 100 megapixels (400 MB as RGBA8) keeps a hostile or
/// absurd image from exhausting the page's memory.
pub const MAX_PIXELS: u64 = 100_000_000;

/// A decoded clipboard image: straight (unpremultiplied) RGBA8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    /// `None` unless the pixels are exactly `width * height` RGBA8 values.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let len = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
        (width > 0 && height > 0 && pixels.len() as u64 == len).then_some(Self { width, height, pixels })
    }
}

/// The index and type of the best image among a clipboard item's (or a paste's) `types`, which
/// browsers list in no useful order (`["text/html", "image/png"]` for Copy Image). Types are
/// compared without parameters and case.
pub fn best_image_type<'a>(types: impl IntoIterator<Item = &'a str>) -> Option<(usize, &'a str)> {
    let rank = |t: &str| {
        let t = t.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        IMAGE_TYPES.iter().position(|k| *k == t).or_else(|| t.starts_with("image/").then_some(IMAGE_TYPES.len()))
    };
    types.into_iter().enumerate().filter_map(|(i, t)| Some((rank(t)?, i, t))).min_by_key(|(r, i, _)| (*r, *i)).map(|(_, i, t)| (i, t))
}

/// Decode clipboard bytes of type `mime` (sniffed: a mislabelled image still decodes) into RGBA8.
/// SVG is rasterized at its own size through the importer, like File › Open.
pub fn decode(mime: &str, bytes: &[u8]) -> Result<Rgba, String> {
    if bytes.is_empty() {
        return Err("the clipboard image is empty".into());
    }
    let svg = mime.to_ascii_lowercase().starts_with("image/svg") || photocraft_io::svg::is_svg(bytes);
    let rgba = if svg && photocraft_codecs::detect(bytes).is_none() {
        let doc = photocraft_io::import("clipboard.svg", bytes).map_err(|e| e.to_string())?.document;
        check_size(doc.size.width, doc.size.height)?;
        let surface = photocraft_compose::flatten_to_surface(&doc, photocraft_doc::PixelFormat::RGBA8, None);
        let mut px = vec![[0u8; 4]; doc.size.width as usize * doc.size.height as usize];
        surface.read_rgba8_into(doc.bounds(), &mut px);
        Rgba::new(doc.size.width, doc.size.height, px.into_iter().flatten().collect())
    } else {
        let mut opts = photocraft_codecs::DecodeOptions::default();
        opts.limits.max_pixels = opts.limits.max_pixels.min(MAX_PIXELS);
        let img = photocraft_codecs::decode_with(bytes, &opts).map_err(|e| e.to_string())?;
        check_size(img.width(), img.height())?;
        Rgba::new(img.width(), img.height(), img.to_rgba8())
    };
    rgba.ok_or_else(|| "the clipboard image has no pixels".into())
}

fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("the clipboard image has no pixels".into());
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("the clipboard image is too large to paste ({w} × {h} pixels)"));
    }
    Ok(())
}

/// The inline image of copied HTML (`<img src="data:image/png;base64,…">`, what some apps and
/// pages copy instead of an image): its type and bytes. Remote `src` URLs aren't fetched.
pub fn html_inline_image(html: &str) -> Option<(String, Vec<u8>)> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower.get(from..)?.find("<img") {
        let tag_start = from + at;
        let tag_end = lower.get(tag_start..)?.find('>').map_or(lower.len(), |e| tag_start + e);
        let tag = lower.get(tag_start..tag_end)?;
        if let Some(src) = tag.find("src=").map(|s| tag_start + s + 4) {
            let quote = html.get(src..src + 1).filter(|q| *q == "\"" || *q == "'");
            let start = src + quote.map_or(0, str::len);
            let value = html.get(start..tag_end)?;
            let end = match quote {
                Some(q) => value.find(q).unwrap_or(value.len()),
                None => value.find(|c: char| c.is_whitespace()).unwrap_or(value.len()),
            };
            if let Some(found) = value.get(..end).and_then(data_url) {
                return Some(found);
            }
        }
        from = tag_end;
    }
    None
}

/// A base64 `data:` URL's type and bytes.
fn data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.trim().strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?.split(';').next()?.trim().to_ascii_lowercase();
    if !mime.starts_with("image/") {
        return None;
    }
    // HTML may wrap a long payload, which base64 doesn't allow: drop the whitespace.
    let payload: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    Some((mime, bytes))
}

/// Is `got`, read back from the browser's clipboard, the image Capishot put there (`ours`)? The
/// browser re-encodes what a page writes, so the colour of transparent pixels and a premultiplied
/// round trip of translucent ones may differ slightly: same size, opacity within 1 and colour
/// within the rounding of its alpha.
pub fn same_image(ours: &Rgba, got: &Rgba) -> bool {
    if ours.width != got.width || ours.height != got.height || ours.pixels.len() != got.pixels.len() {
        return false;
    }
    ours.pixels.as_chunks::<4>().0.iter().zip(got.pixels.as_chunks::<4>().0).all(|(a, b)| {
        let (aa, ba) = (a[3], b[3]);
        if aa.abs_diff(ba) > 1 {
            return false;
        }
        if aa == 0 && ba == 0 {
            return true;
        }
        // Premultiplied at alpha `a`, a channel loses up to 255 / a of precision.
        let tolerance = (255 / u16::from(aa.max(ba).max(1))).max(1) as u8 + 1;
        a.iter().zip(b).take(3).all(|(x, y)| x.abs_diff(*y) <= tolerance)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};

    fn encode(format: Format, w: u32, h: u32, layout: ChannelLayout, px: Vec<u8>) -> Vec<u8> {
        let img = Image::from_u8(w, h, layout, px).unwrap();
        photocraft_codecs::encode(&img, format, &EncodeOptions::default()).unwrap()
    }

    #[test]
    fn png_is_preferred_over_html_and_other_images() {
        assert_eq!(best_image_type(["text/html", "image/png"]), Some((1, "image/png")));
        assert_eq!(best_image_type(["image/jpeg", "text/plain", "image/png"]), Some((2, "image/png")));
        assert_eq!(best_image_type(["IMAGE/WEBP", "image/svg+xml"]), Some((0, "IMAGE/WEBP")));
        assert_eq!(best_image_type(["image/x-unknown", "text/plain"]), Some((0, "image/x-unknown")), "an unknown image type still beats text");
        assert_eq!(best_image_type(["text/plain", "text/html", "Files"]), None);
        assert_eq!(best_image_type([]), None);
    }

    #[test]
    fn decodes_png_with_alpha_and_gray_jpeg_to_rgba8() {
        let px: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 40, 255 - i as u8 * 40, 7, if i == 0 { 0 } else { 200 }]).collect();
        let png = encode(Format::Png, 3, 2, ChannelLayout::Rgba, px.clone());
        let got = decode("image/png", &png).unwrap();
        assert_eq!((got.width, got.height), (3, 2));
        assert_eq!(got.pixels, px);
        // A grey JPEG labelled as PNG: sniffed, expanded to RGBA, opaque.
        let jpg = encode(Format::Jpeg, 4, 4, ChannelLayout::Gray, vec![128; 16]);
        let got = decode("image/png", &jpg).unwrap();
        assert_eq!((got.width, got.height, got.pixels.len()), (4, 4, 64));
        assert!(got.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255 && p[0].abs_diff(128) <= 2 && p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn decodes_svg_text_at_its_size() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#ff0000"/></svg>"##;
        let got = decode("image/svg+xml", svg).unwrap();
        assert_eq!((got.width, got.height), (20, 10));
        let mid = &got.pixels[(5 * 20 + 10) * 4..(5 * 20 + 10) * 4 + 4];
        assert_eq!(mid, &[255, 0, 0, 255]);
    }

    #[test]
    fn garbage_empty_and_oversized_images_are_errors_not_panics() {
        assert!(decode("image/png", b"").is_err());
        assert!(decode("image/png", b"\x89PNG\r\n\x1a\nnot really").is_err());
        assert!(decode("image/svg+xml", b"<svg").is_err());
        assert!(decode("text/plain", &[0u8; 64]).is_err());
        // A PNG header claiming 20000 × 20000 pixels (400 MP) is refused before any allocation.
        let mut big = encode(Format::Png, 1, 1, ChannelLayout::Rgba, vec![0; 4]);
        big[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        big[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert!(decode("image/png", &big).is_err());
        assert!(Rgba::new(2, 2, vec![0; 15]).is_none());
        assert!(Rgba::new(0, 2, vec![]).is_none());
    }

    #[test]
    fn finds_the_inline_image_of_copied_html() {
        let png = encode(Format::Png, 2, 1, ChannelLayout::Rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let html = format!(r#"<meta charset='utf-8'><p>hi</p><IMG alt="x" SRC="data:image/png;base64,{b64}" width=2>"#);
        let (mime, bytes) = html_inline_image(&html).unwrap();
        assert_eq!((mime.as_str(), bytes.as_slice()), ("image/png", png.as_slice()));
        assert_eq!(decode(&mime, &bytes).unwrap().pixels, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let single = format!("<img src='data:image/png;base64,{b64}'>");
        assert!(html_inline_image(&single).is_some());
        // Remote images, non-image data and broken markup are skipped.
        assert!(html_inline_image(r#"<img src="https://example.com/a.png">"#).is_none());
        assert!(html_inline_image(r#"<img src="data:text/plain;base64,aGk=">"#).is_none());
        assert!(html_inline_image("<img src=").is_none());
        assert!(html_inline_image("<img").is_none());
        assert!(html_inline_image("<p>no image</p>").is_none());
        let second = format!(r#"<img src="https://example.com/a.png"><img src="data:image/png;base64,{b64}">"#);
        assert!(html_inline_image(&second).is_some(), "the first inline image, after remote ones");
    }

    #[test]
    fn our_copy_round_tripped_through_the_browser_is_recognised() {
        let ours = Rgba::new(2, 2, vec![10, 20, 30, 255, 200, 100, 50, 128, 9, 9, 9, 0, 0, 0, 0, 3]).unwrap();
        // Chrome re-encodes: transparent colour dropped, translucent colour premultiplied and back.
        let back = Rgba::new(2, 2, vec![10, 20, 30, 255, 201, 99, 50, 128, 0, 0, 0, 0, 85, 0, 0, 3]).unwrap();
        assert!(same_image(&ours, &back));
        let other = Rgba::new(2, 2, vec![10, 20, 40, 255, 200, 100, 50, 128, 9, 9, 9, 0, 0, 0, 0, 3]).unwrap();
        assert!(!same_image(&ours, &other), "an opaque pixel changed: someone else's image");
        let wider = Rgba::new(4, 1, ours.pixels.clone()).unwrap();
        assert!(!same_image(&ours, &wider));
    }
}
