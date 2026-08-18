//! The sidecar as a page, for looking at what the server thinks it has.
//!
//! An annotation is spread across three places — the anchor in the document,
//! the entry in the sidecar, the capture in the store — and a bug in any of them
//! shows up as something missing somewhere else. This draws all three together:
//! one card per annotation, its captures as pictures, its discussion in order,
//! and the block an agent would be handed for it, exactly as `get_annotated_block` returns
//! it.
//!
//! Deliberately plain. It is a debugging view, served beside the annotator
//! rather than inside it, and templated HTML with no script of its own.

use std::sync::Arc;

use super::annotations::{AnnotationRecord, AnnotationServer};
use crate::tool::asset::dev_asset_in;

/// The page shell, with `${...}` placeholders. Read from the source tree when
/// there is one, so it can be edited without rebuilding.
fn shell() -> String {
    dev_asset_in("annos", "sidecar.html", include_str!("../../static/annos/sidecar.html"))
}

/// Text that is going into HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The whole page for one document's sidecar.
pub fn page(title: &str, path: &str, annot: &Arc<dyn AnnotationServer>) -> String {
    let records = match annot.records() {
        Ok(records) => records,
        Err(err) => {
            return shell()
                .replace("${title}", &escape(title))
                .replace("${path}", &escape(path))
                .replace(
                    "${entries}",
                    &format!("<p class=\"none\">{}</p>", escape(&err)),
                );
        }
    };
    let entries = if records.is_empty() {
        "<p class=\"none\">no annotations</p>".to_owned()
    } else {
        records.iter().map(|rec| card(rec, annot)).collect()
    };
    shell()
        .replace("${title}", &escape(title))
        .replace("${path}", &escape(path))
        .replace("${entries}", &entries)
}

/// One annotation.
fn card(rec: &AnnotationRecord, annot: &Arc<dyn AnnotationServer>) -> String {
    let color = if rec.color.starts_with('#') && rec.color.len() == 7 {
        rec.color.clone()
    } else {
        "#9aa0aa".to_owned()
    };
    let flag = |name: &str, on: bool| {
        format!(
            "<span class=\"flag{}\">{name}</span>",
            if on { " on" } else { "" }
        )
    };
    let changed = if rec.mtime.is_empty() || rec.mtime == rec.time {
        String::new()
    } else {
        format!(" · changed {}", escape(&rec.mtime))
    };

    let captures = if rec.captures.is_empty() {
        String::new()
    } else {
        // Which pictures have been drawn on, from the remarks that carry the
        // drawings: a capture holds the picture and nothing about it.
        let scribbled: Vec<&str> = std::iter::once(&rec.scribble)
            .chain(rec.discussion.iter().map(|reply| &reply.scribble))
            .flatten()
            .map(|scribble| scribble.capture.as_str())
            .collect();
        let shots: String = rec
            .captures
            .iter()
            .map(|capture| {
                // Named from the site's root, where the captures are served:
                // this page sits under the document's own URL, which is not
                // where the endpoints are.
                format!(
                    "<figure style=\"margin:0\">\
                       <img src=\"{root}/api/capture/{hash}.{fmt}\" alt=\"capture {hash}\" \
                            width=\"{width}\" height=\"{height}\">\
                       <div class=\"cap\">{time} · {hash} · {width}×{height}{drawn}</div>\
                     </figure>",
                    root = crate::tool::webapp::public_base(),
                    hash = escape(&capture.hash),
                    fmt = escape(&capture.fmt),
                    width = capture.width,
                    height = capture.height,
                    time = escape(&capture.time),
                    drawn = if scribbled.contains(&capture.name()) {
                        " · scribbled on"
                    } else {
                        ""
                    },
                )
            })
            .collect();
        format!("<div class=\"stack\">{shots}</div>")
    };

    let replies = if rec.discussion.is_empty() {
        String::new()
    } else {
        let rows: String = rec
            .discussion
            .iter()
            .map(|reply| {
                format!(
                    "<div class=\"reply\"><span class=\"who\">{who}</span> \
                     <span class=\"meta\">{when}</span>\
                     <div class=\"said\">{said}</div></div>",
                    who = escape(&reply.author),
                    when = escape(&reply.time),
                    said = escape(&reply.content),
                )
            })
            .collect();
        format!("<div class=\"replies\">{rows}</div>")
    };

    // What an agent gets when it asks about this annotation — the same call it
    // makes, so a wrong answer shows up here rather than only in a transcript.
    let block = match annot.block(&rec.uuid, false) {
        Ok(block) => block_fields(&block),
        Err(err) => format!("<div class=\"block\">get_annotated_block failed: {}</div>", escape(&err)),
    };

    format!(
        "<div class=\"anno{resolved}\" style=\"border-left-color:{color}\" id=\"{uuid}\">\
           <div class=\"head\">\
             <span class=\"letter\" style=\"background:{color}\">{letter}</span>\
             <span class=\"uuid\">{uuid}</span>\
             <span class=\"meta\">{rtype} · {scope} · {author} · {time}{changed}</span>\
             <span class=\"flags\">{claimed}{resolved_flag}</span>\
           </div>\
           <div class=\"body\">\
             <div class=\"said\">{content}</div>\
             {captures}\
             {replies}\
             {block}\
           </div>\
         </div>",
        resolved = if rec.resolved { " resolved" } else { "" },
        color = escape(&color),
        letter = escape(&rec.letter),
        uuid = escape(&rec.uuid),
        rtype = escape(&rec.kind),
        scope = escape(rec.location.kind()),
        author = escape(&rec.author),
        time = escape(&rec.time),
        changed = changed,
        claimed = flag("claimed", rec.claimed),
        resolved_flag = flag("resolved", rec.resolved),
        content = escape(&rec.content),
        captures = captures,
        replies = replies,
        block = block,
    )
}

/// The block an annotation sits in, field by field.
///
/// Laid out as the fields of a JSON object rather than as one — a line each,
/// keys aligned, so the shape is readable down the page instead of across it.
/// The source itself is the exception: it is many lines of Typst, and folding
/// it away keeps a card the height of what it says rather than the height of
/// what it points at.
fn block_fields(block: &crate::tool::serve::SourceBlock) -> String {
    let row = |key: &str, value: String| {
        format!("<div class=\"row\"><span class=\"key\">{key}</span>{value}</div>")
    };
    let list = |values: &[String]| {
        let inner: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
        format!("[{}]", inner.join(", "))
    };
    // One row per anchor, named by its index: a block with three of them is
    // three lines that can be read down, not one list to be counted through.
    let anchors: String = if block.anchors.is_empty() {
        row("anchors", "[]".to_owned())
    } else {
        block
            .anchors
            .iter()
            .enumerate()
            .map(|(index, anchor)| {
                row(
                    &format!("anchors[{index}]"),
                    escape(&format!(
                        "{{ref: {:?}, at: {}, label: {:?}, annotations: {:?}}}",
                        anchor.reference, anchor.at, anchor.label, anchor.annotations
                    )),
                )
            })
            .collect()
    };
    let fields = [
        row("blockId", escape(&block.block_id)),
        row("file", escape(&block.file)),
        row("range", format!("{}–{}", block.range.0, block.range.1)),
        row("lines", format!("{}–{}", block.lines.0, block.lines.1)),
        row("headingPath", escape(&list(&block.heading_path))),
        anchors,
    ]
    .concat();
    format!(
        "<div class=\"block\">{fields}{text}</div>",
        text = block_text(&block.block_id, &block.text),
    )
}

/// How many lines of a long block are shown at each end before it is folded.
const SHOWN_LINES: usize = 5;

/// The block's source, shown rather than hidden: it is the thing being talked
/// about, and a card that makes you click to see it is a card you cannot skim.
///
/// A long one is folded to its head and tail with a row of dots between, which
/// toggles the whole thing. The fold is a checkbox and a CSS rule rather than a
/// script — the page has none, and a debug view should not be the reason it
/// gains one.
fn block_text(block_id: &str, text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let folded = lines.len() > SHOWN_LINES * 2;
    // Every line its own element, so the rule that hides the middle can count
    // from both ends.
    let body: String = lines
        .iter()
        .map(|line| format!("<span class=\"ln\">{}\n</span>", escape(line)))
        .collect();
    if !folded {
        return format!("<div class=\"src\"><pre>{body}</pre></div>");
    }
    // The fold is a checkbox and a rule that counts lines from both ends: the
    // page has no script, and a debug view should not be the reason it gains
    // one.
    format!(
        "<div class=\"src folded\">\
           <input type=\"checkbox\" id=\"src-{id}\" class=\"fold\" hidden>\
           <pre>{body}</pre>\
           <label class=\"more\" for=\"src-{id}\">\
             <span class=\"when-folded\">⋯ {hidden} more lines</span>\
             <span class=\"when-open\">⋯ fold back to {shown} lines</span>\
           </label>\
         </div>",
        id = escape(block_id),
        hidden = lines.len() - SHOWN_LINES * 2,
        shown = SHOWN_LINES * 2,
    )
}
