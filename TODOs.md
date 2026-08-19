# TODOs

Known gaps in the annotation tooling, with enough detail to pick each one up
cold.

## Captures of drawings in included files

An annotation on a figure in a file the document `#include`s gets no capture, so
an agent asking about that figure is told there is no picture. Annotating such a
figure works, and so do the block tools; only the picture is missing.

Three things stand in the way, all of them assuming the document is one file:

- `span_range` in `crates/tinymist/src/tool/render/html.rs` returns `None` for a
  span from any file but the main one, so an element from an included file gets
  no `data-typst-src` attribute. The `RenderMap` does carry the file — the
  `Mapper` accepts spans from anywhere and `SrcRange` has a `file` field — so
  this is the attribute alone, not the map.
- `framed_drawings` in the same file recovers what a drawing belongs to by
  reading `data-typst-src` off the enclosing elements. A drawing inside an
  included file therefore has no enclosing range to be attributed to.
- `record_captures` in `crates/tinymist/src/tool/serve/pins.rs` reads
  `world.main()` and matches a drawing to an anchor by comparing offsets in that
  one file.

The fix is to carry the file index through all three: emit `file:start:end` in
the attribute when the file is not the document, have `framed_drawings` return a
`SrcRange` rather than a bare range, and have `record_captures` search every
file the way `compiled_files` in `crates/tinymist/src/tool/serve/annotations.rs`
does.

## Node names that survive a rebuild

`Mapper::uid` in `crates/tinymist/src/tool/render/map.rs` hands out `n1`, `n2`
and so on from a counter, so an element's name depends on how many elements
precede it. Inserting a paragraph renames everything after it. A live server
copes: it holds the map of each rendering it has served, and `relocate`
translates a position taken against an old rendering into the current one.

A statically served document has no server to do that. The page is built once
and published, and an annotation stored against it has nothing but the name to
find its element by, so a rebuild loses every annotation below the first edit.

Deriving the name from the element instead of from its position would fix this.
The ingredients that are stable across a rebuild are the file the element came
from, the path down the document hierarchy to it, its type, and the number of
children it has; a hash of those is a name that only changes when the element
itself does. Siblings can hash alike — three list items of the same shape — so
the name needs a discriminator counting equal siblings, which is stable as long
as the run of them is.

Worth doing for the live case as well: a name that survives a rebuild is also a
name that survives a restart, which is one fewer reason for the server to keep
old renderings.

## `--annotate-fork` does not copy included files

The fork copies the document and its sidecar into a scratch pair, but not the
files the document includes. An agent working on a fork therefore edits the real
included file, which is what a fork exists to prevent.

## Anchors pruned when the sidecar is edited from outside

Editing the sidecar by hand while a server is running has twice removed anchors
from the document: the server reads the file, finds no annotation naming an
anchor, and collects it. An anchor should survive an edit the server did not
make.
