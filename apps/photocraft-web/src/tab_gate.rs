//! One Capishot tab at a time (tab_gate.js): a second tab shows "Use here" instead of starting,
//! and choosing it has the running tab checkpoint its documents and step aside. Capishot's own
//! page only: a page that embeds it, or `?anytab` (tests that open several tabs), runs freely.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/tab_gate.js")]
extern "C" {
    async fn tab_claim(free: bool);
    fn tab_takeover_asked() -> bool;
    fn tab_saving();
    fn tab_release();
}

/// Waits until this page may run (at once when it is the only Capishot tab).
pub async fn claim(free: bool) {
    tab_claim(free).await;
}

/// Another tab chose "Use here".
pub fn takeover_asked() -> bool {
    tab_takeover_asked()
}

/// Still checkpointing the documents for the tab taking over.
pub fn saving() {
    tab_saving();
}

/// Checkpoints written: tell the other tab and reload paused.
pub fn release() {
    tab_release();
}
