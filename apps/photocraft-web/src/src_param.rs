//! `layer.putPixels {src}` from the page (`execute`, `window.photocraft.request`): a Blob, an
//! ArrayBuffer, a typed array or a URL (fetched as the page fetches) becomes the base64 the engine
//! command takes. A base64 string or a `data:` URL goes through as it is.

use base64::Engine as _;
use serde_json::Value;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

fn src_of(params: &JsValue) -> Option<JsValue> {
    if !params.is_object() {
        return None;
    }
    js_sys::Reflect::get(params, &"src".into()).ok().filter(|v| !v.is_undefined() && !v.is_null())
}

/// Whether `params.src` must be read before the command is sent.
pub fn needs_reading(params: &JsValue) -> bool {
    src_of(params).is_some_and(|src| src.as_string().is_none_or(|s| crate::embed_api_core::src_is_url(&s)))
}

fn js_err(e: JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map_or_else(|| format!("{e:?}"), |e| String::from(e.message()))
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_str(url)).await.map_err(js_err)?.dyn_into().map_err(js_err)?;
    if !resp.ok() {
        return Err(format!("layer.putPixels: HTTP {} for {url}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(js_err)?).await.map_err(js_err)?;
    Ok(crate::web::read_array(&js_sys::Uint8Array::new(&buf)).await)
}

/// `params` as JSON, `src` read into base64.
pub async fn read(params: JsValue) -> Result<Value, String> {
    let src = src_of(&params).ok_or("layer.putPixels: no `src`")?;
    let bytes = if let Some(url) = src.as_string() {
        fetch(&url).await?
    } else if let Some(blob) = src.dyn_ref::<web_sys::Blob>() {
        crate::web::read_blob(blob).await?
    } else if src.is_instance_of::<js_sys::ArrayBuffer>() || js_sys::ArrayBuffer::is_view(&src) {
        crate::web::read_array(&js_sys::Uint8Array::new(&src)).await
    } else {
        return Err("layer.putPixels: `src` is base64, a data: URL, a URL, a Blob or an ArrayBuffer".into());
    };
    // The rest of the params as JSON, without the source (it may not stringify).
    let rest = js_sys::Object::assign(&js_sys::Object::new(), params.unchecked_ref());
    js_sys::Reflect::delete_property(&rest, &"src".into()).ok();
    let text = js_sys::JSON::stringify(&rest).ok().and_then(|s| s.as_string()).unwrap_or_else(|| "{}".into());
    let mut v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    v["src"] = Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes));
    Ok(v)
}
