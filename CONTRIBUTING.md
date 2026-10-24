# Contributing to Capishot

Thanks for helping. Capishot is a free, open-source Photoshop replacement for scanlation teams,
built on [PhotoCraft](https://github.com/storytold/photocraft).

## Reporting bugs and ideas

- **Bugs:** open an issue with the steps to reproduce, what you expected and what happened, and
  your browser or OS. Attach a small file that shows the problem if you can, but never share
  pages, raws or fonts you don't have the right to share.
- **Ideas:** describe the scanlation task you're trying to do (cleaning, redrawing,
  typesetting, exporting) and how you do it in Photoshop today.
- **Security issues:** don't open a public issue; see [`SECURITY.md`](SECURITY.md).

## Pull requests

1. Read [`AGENTS.md`](AGENTS.md) and [`docs/contributing.md`](docs/contributing.md): the
   architecture, the "add a command" checklist and the rules below come from there.
2. **Photoshop parity:** menus, tools, shortcuts and behaviour follow Photoshop. Photoshop's
   default shortcuts always win over anything else.
3. **Clean-room:** no code, shaders or assets from Adobe, Photopea or any other proprietary
   editor. Behaviour may be matched from public documentation and observation only.
4. **Rust only:** no JavaScript UI frameworks or webview shells. The web build is the same Rust
   compiled to WebAssembly.
5. **Tests:** every change comes with a test. Run at least the tests of the crates you touched,
   `cargo clippy --workspace --all-targets -- -D warnings` and `cargo xtask wasm`.
6. **Performance matters:** Capishot must stay smooth on phones and machines without a GPU.
   Nothing may block the page for long: heavy work runs as a background job.
7. Crates keep their `photocraft-*` names so upstream merges stay easy; user-facing text says
   Capishot. After merging upstream, run `python3 scripts/capishot-rebrand.py`.

## Licence

By contributing you agree that your contribution is licensed under MIT OR Apache-2.0, like the
rest of the code ([`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE)).
