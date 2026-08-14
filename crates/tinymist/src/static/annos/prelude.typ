// typst-annos v1 — annotation sidecar, edited by humans, tinymist, and agents.
//
// AGENTS: do not hand-edit this file, use the `tinymist` MCP tool to list
// annotations, claim them, add to discussions, and edit the corresponding document.

// Each entry is one annotation, anchored to a cursor label like
// <anno.7C42.math> placed in the document source at the annotated text. The
// entry itself is labeled <anno.7C42>, so one plain search for `<anno.7C42`
// finds both, and `<anno.` finds every annotation in a file.
// Entry shape:
//
//   #metadata((
//     type: str,        // "comment" | "question" | "request"
//     uuid: str,        // e.g. "7C42", matches the anchor id in <anno.7C42.math>
//     scope: str,       // e.g. "math", matches the anchor suffix in <anno.7C42.math>
//     color: str,       // the colour it is drawn in, e.g. "#faa39c"
//     letter: str,      // display letter on the pin: "a".."z", "aa", ...
//     author: str,      // who created it, e.g. "tali" or "claude"
//     content: str,     // the message
//     time: str,        // when it was made, ISO 8601 UTC, e.g. "2026-08-11T01:12:40Z"
//     mtime: str,       // when it last changed: a reply, a flag, a capture
//     claimed: bool,    // somebody is working on it
//     resolved: bool,   // it is done; still drawn, dimmed, until deleted
//     captures: (),     // for graphical annotations, what the thing it points at
//                       // has looked like, oldest first, each:
//                       // (time: str, fmt: str, hash: str,
//                       //  width: int, height: int, markup: str?)
//                       // fmt is how it is stored ("svg" today); width and height
//                       // are its size on the page in CSS pixels; markup is what
//                       // the reader drew on top, as inline SVG. Ask the MCP tool
//                       // `get_capture` for the picture itself.
//     discussion: (),   // ordered replies, each:
//                       // (author: str, time: str, content: str)
//   )) <anno.7C42>
