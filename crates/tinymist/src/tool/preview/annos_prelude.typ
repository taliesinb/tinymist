// typst-annos v1 — annotation sidecar, edited by humans, tinymist, and agents.
//
// Each entry is one annotation, anchored to a cursor label like
// <anno.7C42.math> placed in the document source at the annotated text. The
// entry itself is labeled <anno.7C42>, so one plain search for `<anno.7C42`
// finds both, and `<anno.` finds every annotation in a file. (A leading dot,
// `<.7C42.math>`, is not a label in Typst — it stays text and renders.)
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
//     time: str,        // ISO 8601 UTC, e.g. "2026-08-11T01:12:40Z"
//     status: str,      // "created" | "ongoing" | "resolved"
//     discussion: (),   // ordered replies, each:
//                       //   (author: str, time: str, content: str)
//   )) <anno.7C42>
//
// Rules for agents:
// - Read this file with `typst query <file> metadata` — it returns every
//   entry as JSON. tinymist reads it the same way (by evaluation), so
//   entries may use any valid Typst, not just literal dicts; only keep the
//   trailing <anno.XXXX> label directly after an entry's closing `))`.
// - Reply by appending to `discussion`; never edit another author's text.
// - Flip `status` to "ongoing" while addressing an entry and "resolved"
//   when done. The preview renders resolved annotations dimmed.
// - Do not touch `type`, `uuid`, `scope`, `author`, `content`, or `time` of
//   existing entries.
// - Typst syntax notes: an empty array is (); a one-element array of dicts
//   needs a trailing comma: ((author: "x", ...),).
// - Find an anchor's position with `typst query main.typ "<anno.7C42.math>"`,
//   or just search the source for `<anno.7C42`; deleting an entry or its
//   anchor orphans the other half harmlessly.
