# capishot-host changelog

Every change to the stable API is listed here with the commit to pin. Breaking changes are under
**Breaking** and bump `API_VERSION`. Experimental items (`live`, `Host::session`,
`Host::session_mut`, `unstable`) may change in any commit; their changes are listed under
**Experimental**.

## API_VERSION 1 (first release)

- `Host`: `execute`, `execute_with`, `execute_request`, `commands`; jobs (`start`, `job`, `jobs`,
  `cancel`, native `wait`); `poll`, `subscribe`, `unsubscribe`, `events`; documents (`documents`,
  `active_document`, `open`, `open_psd`, `start_open`, `show`, `prewarm`, `close`, `save`,
  `save_psd`, `start_save`, `take_saved`, `inspect`); pixels (`get_pixels`, `put_pixels`);
  `batch`, `undo`, `redo`, `begin_group`, `end_group`, `group`, `in_group`; views (`views`,
  `define_view`, `apply_view`); tools (`tool`, `set_tool`); `set_view_hooks` / `ViewHooks`.
- `Event` (serialises as the web embed's events), `Origin`, `JobEnd`, `Error`, ids
  (`DocumentId`, `LayerId`, `LayerUid`, `JobHandle`) and the option/result structs.
- **Experimental**: `live` (`Op`, `Envelope`, `Applied`; `Host::live_start`, `live_stop`,
  `is_live`, `live_apply`, `live_take_local`, `layer_uid`).
