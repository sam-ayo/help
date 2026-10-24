# md_editor

The native Markdown editor of the document panel (`crates/app/src/ui/doc.rs`).

## Origin

This crate is a copy of `gpui-markdown-editor` from
[yuWorm/gpui-markdeditor](https://github.com/yuWorm/gpui-markdeditor) (commit `a422e2c`).
That crate is the editor core of [Velotype](https://github.com/manyougz/velotype) by
`manyougz`, as a separate component. Both are under the Apache License 2.0. See
`LICENSE-APACHE`.

## Changes in this copy

- The package name is `md_editor`.
- The crate uses `gpui-pre 0.3.9`, not `gpui 0.2.2`. The API changes are the same as in
  Velotype commit `ce5d2b1`: `focus(window, cx)`, `blur(cx)`, `flex_grow(1.0)`,
  `max_offset().y`, `ShapedLine::paint` with `TextAlign` and a width, and the new
  `ClipboardEntry::ExternalPaths` and `ImageFormat` variants.
- `MarkdownEditorOptions::autofocus`: when it is `false`, the editor does not take the
  keyboard focus when it opens or when it loads new text.
- All the bindings of `default_key_bindings` are in the editor context. The `cmd-tab`
  binding is removed, because macOS uses it.
- The clippy lint settings come from the `Cargo.toml` of Velotype.
- Row culling is off (`CULL_ROWS` in `src/editor/render.rs`). In this version, the
  culling gives the rows after a long paragraph a width of 0, so their text goes out
  of view (Velotype issue #106). Velotype fixed it in pull request #115, which is
  newer than this copy. The test `rows_after_a_long_paragraph_keep_their_width`
  checks this.
