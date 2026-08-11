// typst-annos v1 — annotation sidecar, edited by humans, tinymist, and agents.
//
// Each entry is one annotation, anchored to a cursor label like <-7C42->
// placed in the document source at the annotated text. The entry itself is
// labeled <note-7C42>. Entry shape:
//
//   #metadata((
//     type: str,        // "comment" | "question" | "request"
//     label: str,       // "7C42", matches the anchor label <-7C42->
//     author: str,      // who created it, e.g. "tali" or "agent"
//     content: str,     // the message
//     time: str,        // ISO 8601 UTC, e.g. "2026-08-11T01:12:40Z"
//     status: str,      // "created" | "ongoing" | "resolved"
//     discussion: (),   // ordered replies, each:
//                       //   (author: str, time: str, content: str)
//   )) <note-7C42>
//
// Rules for agents:
// - Read this file with `typst query <file> metadata` — it returns every
//   entry as JSON. tinymist reads it the same way (by evaluation), so
//   entries may use any valid Typst, not just literal dicts; only keep the
//   trailing <note-XXXX> label directly after an entry's closing `))`.
// - Reply by appending to `discussion`; never edit another author's text.
// - Flip `status` to "ongoing" while addressing an entry and "resolved"
//   when done. The preview renders resolved annotations gray.
// - Do not touch `type`, `label`, `author`, `content`, or `time` of
//   existing entries.
// - Typst syntax notes: an empty array is (); a one-element array of dicts
//   needs a trailing comma: ((author: "x", ...),).
// - Find an anchor's position with `typst query main.typ "<-7C42->"`;
//   deleting an entry or its anchor orphans the other half harmlessly.

