use crate::location::*;
use crate::record::*;
use crate::sidecar::*;

fn word(label: &str) -> TypstWordRef {
    TypstWordRef {
        label: label.into(),
    }
}

fn cursor(label: &str, side: HSide) -> TypstTextCursorRef {
    TypstTextCursorRef {
        label: label.into(),
        side,
    }
}

fn node(label: &str) -> TypstNodeRef {
    TypstNodeRef {
        label: label.into(),
    }
}

fn edge(label: &str, side: VSide) -> TypstNodeCursorRef {
    TypstNodeCursorRef {
        label: label.into(),
        side,
    }
}

/// Every location, so that a variant added without a test is a variant this
/// notices.
fn every_typst_location() -> Vec<TypstLocation> {
    vec![
        Location::Word {
            reference: word("anno.A000"),
        },
        Location::Line {
            reference: word("anno.A001"),
        },
        Location::Sentence {
            reference: word("anno.A002"),
        },
        Location::PosH {
            reference: cursor("anno.A003", HSide::Left),
        },
        Location::PosV {
            reference: edge("anno.A004", VSide::Top),
        },
        Location::SpanH {
            begin: cursor("anno.A005", HSide::Left),
            end: cursor("anno.A006", HSide::Right),
        },
        Location::SpanV {
            begin: edge("anno.A007", VSide::Top),
            end: edge("anno.A008", VSide::Bottom),
        },
        Location::Raw {
            reference: node("anno.A009"),
        },
        Location::Para {
            reference: node("anno.A00a"),
        },
        Location::Item {
            reference: node("anno.A00b"),
        },
        Location::Block {
            reference: node("anno.A00c"),
        },
        Location::Opaque {
            reference: node("anno.A00d"),
        },
        Location::Math {
            reference: node("anno.A00e"),
        },
        Location::MathBlock {
            reference: node("anno.A00f"),
        },
        Location::Link {
            reference: node("anno.A010"),
        },
        Location::Svg {
            reference: node("anno.A011"),
        },
    ]
}

#[test]
fn typst_locations_round_trip() {
    for location in every_typst_location() {
        let json = serde_json::to_string(&location).expect("serialises");
        let back: TypstLocation = serde_json::from_str(&json).expect("reads back");
        assert_eq!(location, back, "{json}");
    }
}

#[test]
fn html_locations_round_trip() {
    let locations: Vec<HtmlLocation> = vec![
        Location::Word {
            reference: HtmlWordRef {
                node: "t12".into(),
                beg: 4,
                end: 11,
                w: Some("monoidal".into()),
            },
        },
        Location::PosH {
            reference: HtmlTextCursorRef {
                node: "t12".into(),
                pos: 11,
                l: Some("monoidal".into()),
                r: Some(" category".into()),
            },
        },
        Location::PosV {
            reference: HtmlNodeCursorRef {
                node: "b3".into(),
                side: VSide::Bottom,
            },
        },
        Location::SpanH {
            begin: HtmlTextCursorRef {
                node: "t12".into(),
                pos: 0,
                l: None,
                r: None,
            },
            end: HtmlTextCursorRef {
                node: "t13".into(),
                pos: 7,
                l: None,
                r: None,
            },
        },
        Location::Svg {
            reference: HtmlNodeRef {
                node: "tm-4".into(),
            },
        },
    ];
    for location in locations {
        let json = serde_json::to_string(&location).expect("serialises");
        let back: HtmlLocation = serde_json::from_str(&json).expect("reads back");
        assert_eq!(location, back, "{json}");
    }
}

/// The wire shape, spelled out: a change to any of these names is a change
/// every client sees, so it should be a change somebody made on purpose.
#[test]
fn locations_have_the_agreed_shape() {
    let location: TypstLocation = Location::SpanH {
        begin: cursor("anno.A005", HSide::Left),
        end: cursor("anno.A006", HSide::Right),
    };
    assert_eq!(
        serde_json::to_value(&location).unwrap(),
        serde_json::json!({
            "type": "span.h",
            "begin": {"type": "text_cursor", "ref": "anno.A005", "side": "left"},
            "end": {"type": "text_cursor", "ref": "anno.A006", "side": "right"},
        })
    );

    let word: HtmlLocation = Location::Word {
        reference: HtmlWordRef {
            node: "t12".into(),
            beg: 4,
            end: 11,
            w: Some("monoid".into()),
        },
    };
    assert_eq!(
        serde_json::to_value(&word).unwrap(),
        serde_json::json!({
            "type": "word",
            "ref": {"type": "word", "ref": "t12", "beg": 4, "end": 11, "w": "monoid"},
        })
    );
}

#[test]
fn context_is_optional() {
    let bare: HtmlLocation = serde_json::from_value(serde_json::json!({
        "type": "pos.h",
        "ref": {"type": "text_cursor", "ref": "t1", "pos": 3},
    }))
    .expect("reads a cursor with no context");
    let Location::PosH { reference } = bare else {
        panic!("wrong variant")
    };
    assert_eq!(reference.l, None);
    assert_eq!(reference.r, None);
    // And it is left out again on the way back, rather than written as null.
    let json = serde_json::to_string(&Location::<Html>::PosH { reference }).unwrap();
    assert!(!json.contains("null"), "{json}");
}

#[test]
fn a_location_names_its_anchors() {
    let span: TypstLocation = Location::SpanH {
        begin: cursor("anno.A005", HSide::Left),
        end: cursor("anno.A006", HSide::Right),
    };
    assert_eq!(span.labels(), vec!["anno.A005", "anno.A006"]);
    assert!(span.is_span());
    assert!(!span.is_position());

    let point: TypstLocation = Location::PosH {
        reference: cursor("anno.A003", HSide::Left),
    };
    assert!(point.is_position());
    assert_eq!(point.kind(), "pos.h");
}

fn annotation(uuid: &str, location: TypstLocation) -> Annotation {
    Annotation {
        uuid: uuid.into(),
        letter: "a".into(),
        location,
        snapshot: Some("monoid".into()),
        kind: "comment".into(),
        color: "#faa39c".into(),
        author: "tali".into(),
        time: "2026-08-16T09:10:00Z".into(),
        mtime: "2026-08-16T09:10:00Z".into(),
        claimed: false,
        resolved: false,
        content: "is this the right word?".into(),
        discussion: vec![],
        captures: vec![],
    }
}

#[test]
fn a_sidecar_round_trips() {
    let mut sidecar = Sidecar::default();
    sidecar.put(annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    ));
    sidecar.put(annotation(
        "8ac41d90ff2b6e35",
        Location::Svg {
            reference: node("anno.A011"),
        },
    ));
    let json = sidecar.to_json();
    let back = Sidecar::parse(&json).expect("reads back");
    assert_eq!(sidecar, back);
    assert_eq!(back.version, VERSION);
    assert!(json.ends_with('\n'));
}

#[test]
fn writing_the_same_annotations_writes_the_same_file() {
    let mut one = Sidecar::default();
    let mut two = Sidecar::default();
    for sidecar in [&mut one, &mut two] {
        sidecar.put(annotation(
            "3f1a9c22b4d0e7a1",
            Location::Word {
                reference: word("anno.A000"),
            },
        ));
        sidecar.put(annotation(
            "8ac41d90ff2b6e35",
            Location::PosV {
                reference: edge("anno.A004", VSide::Top),
            },
        ));
    }
    assert_eq!(one.to_json(), two.to_json());
}

#[test]
fn putting_twice_replaces_rather_than_repeats() {
    let mut sidecar = Sidecar::default();
    sidecar.put(annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    ));
    let mut changed = annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    );
    changed.content = "changed my mind".into();
    sidecar.put(changed);
    assert_eq!(sidecar.annotations.len(), 1);
    assert_eq!(sidecar.find("3f1a9c22b4d0e7a1").unwrap().content, "changed my mind");
    assert!(sidecar.remove("3f1a9c22b4d0e7a1"));
    assert!(!sidecar.remove("3f1a9c22b4d0e7a1"));
}

#[test]
fn anchors_in_use_are_counted_once_each() {
    let mut sidecar = Sidecar::default();
    // Two annotations on the same word: the thing the old scheme could not do,
    // since a Typst element carries one label.
    sidecar.put(annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    ));
    sidecar.put(annotation(
        "8ac41d90ff2b6e35",
        Location::Sentence {
            reference: word("anno.A000"),
        },
    ));
    sidecar.put(annotation(
        "b0c2e5417d38a96f",
        Location::SpanH {
            begin: cursor("anno.A001", HSide::Left),
            end: cursor("anno.A002", HSide::Right),
        },
    ));
    assert_eq!(
        sidecar.labels_in_use(),
        vec!["anno.A000", "anno.A001", "anno.A002"]
    );
}

#[test]
fn a_missing_sidecar_reads_as_an_empty_one() {
    let dir = std::env::temp_dir().join("tinymist-annos-tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("nothing-here.annos.json");
    let _ = std::fs::remove_file(&path);
    assert_eq!(Sidecar::read(&path).expect("reads"), Sidecar::default());
    assert_eq!(Sidecar::parse("   ").expect("reads"), Sidecar::default());
}

#[test]
fn a_sidecar_is_written_and_read_back() {
    let dir = std::env::temp_dir().join("tinymist-annos-tests");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("round-trip.annos.json");
    let mut sidecar = Sidecar::default();
    sidecar.put(annotation(
        "3f1a9c22b4d0e7a1",
        Location::Math {
            reference: node("anno.A00e"),
        },
    ));
    sidecar.write(&path).expect("writes");
    assert_eq!(Sidecar::read(&path).expect("reads"), sidecar);
    // Nothing left behind by the write.
    assert!(!path.with_extension("json.tmp").exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_document_says_where_its_annotations_are_kept() {
    let path = sidecar_path(std::path::Path::new("/docs/math.typ"));
    assert_eq!(path, std::path::Path::new("/docs/math.annos.json"));
    assert!(is_sidecar(&path));
    assert!(!is_sidecar(std::path::Path::new("/docs/math.typ")));
}

#[test]
fn anchor_labels_are_recognisable() {
    assert_eq!(crate::anchor_label("7C42"), "anno.7C42");
    assert_eq!(crate::anchor_id("anno.7C42"), Some("7C42"));
    assert_eq!(crate::anchor_id("intro"), None);
}

#[test]
fn an_anchor_nothing_points_at_is_collectable() {
    let mut sidecar = Sidecar::default();
    sidecar.put(annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    ));
    // The document has one anchor nobody wants, and is missing one somebody
    // does: the two directions, which are different problems.
    let report = crate::audit(["anno.A000", "anno.A0ff"], &sidecar);
    assert_eq!(report.unused, vec!["anno.A0ff"]);
    assert!(report.missing.is_empty());
    assert!(!report.is_clean());

    let report = crate::audit(["anno.A0ff"], &sidecar);
    assert_eq!(report.missing, vec!["anno.A000"]);
    let lost = crate::dangling(&sidecar, &report);
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].0.uuid, "3f1a9c22b4d0e7a1");
    assert_eq!(lost[0].1, vec!["anno.A000"]);
    // What it was about, for a reader who now cannot see it.
    assert_eq!(lost[0].0.snapshot.as_deref(), Some("monoid"));

    assert!(crate::audit(["anno.A000"], &sidecar).is_clean());
}

#[test]
fn a_shared_anchor_survives_one_of_its_annotations() {
    let mut sidecar = Sidecar::default();
    for uuid in ["3f1a9c22b4d0e7a1", "8ac41d90ff2b6e35"] {
        sidecar.put(annotation(
            uuid,
            Location::Word {
                reference: word("anno.A000"),
            },
        ));
    }
    sidecar.remove("3f1a9c22b4d0e7a1");
    // Still in use by the other one, so not litter.
    assert!(crate::audit(["anno.A000"], &sidecar).is_clean());
    sidecar.remove("8ac41d90ff2b6e35");
    assert_eq!(
        crate::audit(["anno.A000"], &sidecar).unused,
        vec!["anno.A000"]
    );
}

#[test]
fn letters_are_the_servers_to_give() {
    let mut sidecar = Sidecar::default();
    assert_eq!(sidecar.next_letter(), "a");

    let mut first = annotation(
        "3f1a9c22b4d0e7a1",
        Location::Word {
            reference: word("anno.A000"),
        },
    );
    first.letter = sidecar.next_letter();
    sidecar.put(first);
    assert_eq!(sidecar.next_letter(), "b");

    let mut second = annotation(
        "8ac41d90ff2b6e35",
        Location::Word {
            reference: word("anno.A001"),
        },
    );
    second.letter = sidecar.next_letter();
    sidecar.put(second);
    assert_eq!(sidecar.next_letter(), "c");

    // A letter freed by a removal is given out again, rather than counted past.
    sidecar.remove("3f1a9c22b4d0e7a1");
    assert_eq!(sidecar.next_letter(), "a");
}

#[test]
fn letters_go_past_z() {
    let mut sidecar = Sidecar::default();
    for n in 0..26 {
        let mut anno = annotation(
            &format!("{n:016x}"),
            Location::Word {
                reference: word("anno.A000"),
            },
        );
        anno.letter = sidecar.next_letter();
        sidecar.put(anno);
    }
    assert_eq!(sidecar.next_letter(), "aa");
}

#[test]
fn a_render_map_turns_characters_into_source() {
    use crate::render_map::*;

    // One run in three pieces: a label sat in the middle of it, so the
    // characters are contiguous and the source offsets are not.
    let entry = NodeEntry {
        kind: NodeKind::Text,
        range: None,
        segments: vec![
            Segment { at: 0, len: 10, file: 0, offset: 0 },
            Segment { at: 10, len: 1, file: 0, offset: 10 },
            Segment { at: 11, len: 12, file: 0, offset: 15 },
        ],
    };
    assert_eq!(entry.text_len(), 23);
    assert_eq!(entry.source_of(0), Some((0, 0)));
    assert_eq!(entry.source_of(9), Some((0, 9)));
    // Across the gap the label left.
    assert_eq!(entry.source_of(11), Some((0, 15)));
    assert_eq!(entry.source_of(12), Some((0, 16)));
    // The very end of the run is a place, not a failure.
    assert_eq!(entry.source_of(23), Some((0, 27)));
    assert_eq!(entry.source_of(24), None);

    let mut map = RenderMap {
        render: "3f1a9c22".into(),
        files: vec![FileEntry { path: "/docs/math.typ".into(), hash: "abc".into() }],
        nodes: Default::default(),
    };
    map.nodes.insert("t1".into(), entry);
    let back = RenderMap::parse(&map.to_json()).expect("reads back");
    assert_eq!(map, back);
    assert_eq!(back.document().unwrap().path, "/docs/math.typ");
    assert!(back.node("t1").is_some());
    assert!(back.node("t2").is_none());
}

#[test]
fn kinds_know_what_they_can_be_asked() {
    use crate::render_map::NodeKind;
    assert!(NodeKind::Block.is_block());
    assert!(NodeKind::Svg.is_block());
    assert!(!NodeKind::Text.is_block());
    assert!(NodeKind::Text.has_text());
    assert!(!NodeKind::Math.has_text());
}

#[test]
fn an_unchanged_file_translates_exactly() {
    use crate::migrate::*;
    let text = "one\ntwo\nthree\n";
    let rebase = Rebase::between(text, text);
    assert!(rebase.is_identity());
    assert_eq!(rebase.at(0), Shift::At(0));
    assert_eq!(rebase.at(7), Shift::At(7));
    assert_eq!(rebase.at(text.len()), Shift::At(text.len()));
}

#[test]
fn text_after_an_insertion_moves_by_its_length() {
    use crate::migrate::*;
    let old = "alpha\nbeta\ngamma\n";
    let new = "alpha\nINSERTED\nbeta\ngamma\n";
    let rebase = Rebase::between(old, new);
    // Before the insertion: unmoved.
    assert_eq!(rebase.at(0), Shift::At(0));
    assert_eq!(rebase.at(3), Shift::At(3));
    // After it: moved by the length of the inserted line.
    assert_eq!(rebase.at(6), Shift::At(6 + "INSERTED\n".len()));
    assert_eq!(rebase.at(11), Shift::At(11 + "INSERTED\n".len()));
}

#[test]
fn a_position_in_deleted_text_is_lost() {
    use crate::migrate::*;
    let old = "alpha\nbeta\ngamma\n";
    let new = "alpha\ngamma\n";
    let rebase = Rebase::between(old, new);
    assert_eq!(rebase.at(0), Shift::At(0));
    // Inside "beta", which is gone.
    assert_eq!(rebase.at(8), Shift::Lost);
    // "gamma" is still there, earlier than it was.
    assert_eq!(rebase.at(11), Shift::At(6));
}

#[test]
fn two_separate_edits_keep_the_text_between_them() {
    use crate::migrate::*;
    let old = "one\ntwo\nthree\nfour\nfive\n";
    let new = "one\nTWO\nthree\nfour\nFIVE\n";
    let rebase = Rebase::between(old, new);
    // "three" and "four" sit between the two changes and are unaffected. A
    // translation built only from a shared head and tail would lose them.
    let three = old.find("three").unwrap();
    assert_eq!(rebase.at(three), Shift::At(new.find("three").unwrap()));
    let four = old.find("four").unwrap();
    assert_eq!(rebase.at(four), Shift::At(new.find("four").unwrap()));
    // A position inside a changed line is lost. The boundary between two lines
    // is not: it belongs to the line that follows, which is kept or not on its
    // own account.
    assert_eq!(rebase.at(old.find("two").unwrap() + 1), Shift::Lost);
}

#[test]
fn a_stored_rendering_translates_positions_after_a_restart() {
    use crate::render_map::*;
    use crate::store::*;

    let dir = std::env::temp_dir().join("tinymist-annos-tests/store");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::new(dir.clone());

    let was = "alpha\nbeta\ngamma\n";
    let now = "alpha\nINSERTED\nbeta\ngamma\n";
    let stored = StoredRender {
        map: RenderMap {
            render: "abc123".into(),
            files: vec![FileEntry {
                path: "/docs/x.typ".into(),
                hash: "h".into(),
            }],
            nodes: Default::default(),
        },
        text: was.into(),
        stored: "2026-08-16T09:10:00Z".into(),
    };
    store.put(&stored).expect("stores");
    // Storing the same rendering again is not an error.
    store.put(&stored).expect("stores again");
    assert_eq!(store.held().len(), 1);
    assert_eq!(store.get("abc123").map(|held| held.text), Some(was.into()));

    // A position taken against the stored rendering, read against the file as
    // it is now.
    assert_eq!(
        resolve_offset(&store, "abc123", 6, now),
        Resolved::At(6 + "INSERTED\n".len())
    );
    // A rendering nobody kept.
    assert_eq!(resolve_offset(&store, "ffffff", 6, now), Resolved::Unknown);
    // A position in text that has since gone.
    assert_eq!(
        resolve_offset(&store, "abc123", 8, "alpha\ngamma\n"),
        Resolved::Lost
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_keeps_only_so_many_renderings() {
    use crate::render_map::*;
    use crate::store::*;

    let dir = std::env::temp_dir().join("tinymist-annos-tests/prune");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::new(dir.clone());
    for n in 0..KEEP + 4 {
        let stored = StoredRender {
            map: RenderMap {
                render: format!("r{n:04}"),
                files: vec![],
                nodes: Default::default(),
            },
            text: format!("version {n}"),
            stored: String::new(),
        };
        store.put(&stored).expect("stores");
        // Modification times have whole-second granularity on some
        // filesystems; keep the order unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(store.held().len(), KEEP);
    // The oldest went; the newest stayed.
    assert!(store.get("r0000").is_none());
    assert!(store.get(&format!("r{:04}", KEEP + 3)).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- resolving -------------------------------------------------------------

/// A document, its rendering map, and the pieces a conversion needs.
fn fixture(text: &str) -> (typst_syntax::Source, crate::render_map::RenderMap) {
    use crate::render_map::*;
    let source = typst_syntax::Source::detached(text.to_owned());
    let map = RenderMap {
        render: "r1".into(),
        files: vec![FileEntry {
            path: "/docs/x.typ".into(),
            hash: "h".into(),
        }],
        nodes: Default::default(),
    };
    (source, map)
}

fn run_node(
    map: &mut crate::render_map::RenderMap,
    uid: &str,
    kind: crate::render_map::NodeKind,
    text_at: usize,
    len: usize,
) {
    use crate::render_map::*;
    map.nodes.insert(
        uid.into(),
        NodeEntry {
            kind,
            range: Some(SrcRange {
                file: 0,
                start: text_at,
                end: text_at + len,
            }),
            segments: vec![Segment {
                at: 0,
                len,
                file: 0,
                offset: text_at,
            }],
        },
    );
}

#[test]
fn a_word_resolves_to_a_new_anchor_after_it() {
    use crate::render_map::NodeKind;
    let text = "The monoid is here.\n";
    let (source, mut map) = fixture(text);
    run_node(&mut map, "n1", NodeKind::Text, 0, text.len() - 1);

    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 7,
    };
    let asked = Location::Word {
        reference: HtmlWordRef {
            node: "n1".into(),
            beg: 4,
            end: 10,
            w: Some("monoid".into()),
        },
    };
    let out = crate::resolve::resolve(&ctx, &asked).expect("resolves");
    assert!(!out.coarsened);
    assert_eq!(out.edits.len(), 1);
    // The label goes directly after the word.
    assert_eq!(out.edits[0].at, 10);
    let Location::Word { reference } = &out.location else {
        panic!("wrong variant")
    };
    assert_eq!(out.edits[0].text, format!("<{}>", reference.label));
    assert!(reference.label.starts_with("anno."));
    let written = crate::anchor::apply(text, &out.edits);
    assert!(written.starts_with("The monoid<anno."), "{written}");
}

#[test]
fn a_second_annotation_on_the_same_word_shares_the_anchor() {
    use crate::render_map::NodeKind;
    let text = "The monoid<anno.A1B2> is here.\n";
    let (source, mut map) = fixture(text);
    run_node(&mut map, "n1", NodeKind::Text, 0, 10);

    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 7,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::Word {
            reference: HtmlWordRef {
                node: "n1".into(),
                beg: 4,
                end: 10,
                w: Some("monoid".into()),
            },
        },
    )
    .expect("resolves");
    // Nothing is written, and the existing anchor is used.
    assert!(out.edits.is_empty());
    let Location::Word { reference } = &out.location else {
        panic!("wrong variant")
    };
    assert_eq!(reference.label, "anno.A1B2");
}

#[test]
fn a_position_inside_a_word_snaps_to_the_end_of_it() {
    use crate::render_map::NodeKind;
    let text = "The monoid is here.\n";
    let (source, mut map) = fixture(text);
    run_node(&mut map, "n1", NodeKind::Text, 0, text.len() - 1);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 1,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::PosH {
            reference: HtmlTextCursorRef {
                node: "n1".into(),
                // Between "mon" and "oid".
                pos: 7,
                l: None,
                r: None,
            },
        },
    )
    .expect("resolves");
    assert_eq!(out.edits[0].at, 10);
    let Location::PosH { reference } = &out.location else {
        panic!("wrong variant")
    };
    // The position is to the right of the label, which sits after the word.
    assert_eq!(reference.side, HSide::Right);
}

#[test]
fn a_position_in_a_call_is_answered_with_the_call() {
    use crate::render_map::NodeKind;
    let text = "Before #emph[inside] after.\n";
    let (source, mut map) = fixture(text);
    // The rendered run "inside" comes from within the call.
    run_node(&mut map, "n1", NodeKind::Text, 13, 6);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 3,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::Word {
            reference: HtmlWordRef {
                node: "n1".into(),
                beg: 0,
                end: 6,
                w: Some("inside".into()),
            },
        },
    )
    .expect("resolves");
    // Inside a content block a label is valid, so this is not coarsened; the
    // label lands after the word within the call.
    assert_eq!(&text[..out.edits[0].at], "Before #emph[inside");
}

#[test]
fn a_span_across_two_places_writes_two_anchors() {
    use crate::render_map::NodeKind;
    let text = "alpha beta gamma delta\n";
    let (source, mut map) = fixture(text);
    run_node(&mut map, "n1", NodeKind::Text, 0, text.len() - 1);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 5,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::SpanH {
            begin: HtmlTextCursorRef {
                node: "n1".into(),
                pos: 6,
                l: None,
                r: None,
            },
            end: HtmlTextCursorRef {
                node: "n1".into(),
                pos: 16,
                l: None,
                r: None,
            },
        },
    )
    .expect("resolves");
    assert_eq!(out.edits.len(), 2);
    let Location::SpanH { begin, end } = &out.location else {
        panic!("wrong variant")
    };
    assert_ne!(begin.label, end.label);
    assert_eq!(begin.side, HSide::Right);
    assert_eq!(end.side, HSide::Left);
    let written = crate::anchor::apply(text, &out.edits);
    assert!(written.contains("beta<anno."), "{written}");
}

#[test]
fn a_position_is_translated_when_the_document_has_moved_on() {
    use crate::render_map::NodeKind;
    let was = "The monoid is here.\n";
    let now = "A new first line.\nThe monoid is here.\n";
    let (source, mut map) = fixture(now);
    run_node(&mut map, "n1", NodeKind::Text, 0, was.len() - 1);
    let ctx = crate::resolve::Context {
        map: &map,
        was,
        source: &source,
        seed: 9,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::Word {
            reference: HtmlWordRef {
                node: "n1".into(),
                beg: 4,
                end: 10,
                w: Some("monoid".into()),
            },
        },
    )
    .expect("resolves");
    // The word is now eighteen characters further into the file.
    assert_eq!(out.edits[0].at, 10 + "A new first line.\n".len());
    let written = crate::anchor::apply(now, &out.edits);
    assert!(written.contains("The monoid<anno."), "{written}");
}

#[test]
fn a_position_in_deleted_text_does_not_resolve() {
    use crate::render_map::NodeKind;
    let was = "The monoid is here.\n";
    let now = "Something else entirely.\n";
    let (source, mut map) = fixture(now);
    run_node(&mut map, "n1", NodeKind::Text, 0, was.len() - 1);
    let ctx = crate::resolve::Context {
        map: &map,
        was,
        source: &source,
        seed: 9,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::Word {
            reference: HtmlWordRef {
                node: "n1".into(),
                beg: 4,
                end: 10,
                w: Some("monoid".into()),
            },
        },
    );
    assert_eq!(out, Err(crate::resolve::Failure::Lost));
}

#[test]
fn a_vertical_position_needs_a_block() {
    use crate::render_map::NodeKind;
    let text = "Some text here.\n";
    let (source, mut map) = fixture(text);
    run_node(&mut map, "n1", NodeKind::Text, 0, 15);
    run_node(&mut map, "n2", NodeKind::Block, 0, 15);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 2,
    };
    let against_text = crate::resolve::resolve(
        &ctx,
        &Location::PosV {
            reference: HtmlNodeCursorRef {
                node: "n1".into(),
                side: VSide::Top,
            },
        },
    );
    assert_eq!(
        against_text,
        Err(crate::resolve::Failure::WrongKind("n1".into()))
    );

    let against_block = crate::resolve::resolve(
        &ctx,
        &Location::PosV {
            reference: HtmlNodeCursorRef {
                node: "n2".into(),
                side: VSide::Top,
            },
        },
    )
    .expect("resolves against a block");
    let Location::PosV { reference } = &against_block.location else {
        panic!("wrong variant")
    };
    assert_eq!(reference.side, VSide::Top);
}

#[test]
fn an_unknown_node_is_reported_as_such() {
    let text = "Some text here.\n";
    let (source, map) = fixture(text);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 2,
    };
    let out = crate::resolve::resolve(
        &ctx,
        &Location::Svg {
            reference: HtmlNodeRef { node: "n9".into() },
        },
    );
    assert_eq!(out, Err(crate::resolve::Failure::NoSuchNode("n9".into())));
}

#[test]
fn a_location_makes_the_round_trip() {
    use crate::render_map::NodeKind;
    // A document with the anchor already in it, as it would be after a first
    // annotation was made.
    let text = "The monoid<anno.A1B2> is here.\n";
    let (source, mut map) = fixture(text);
    // The run before the label, and the run after it: a label splits a run.
    run_node(&mut map, "n1", NodeKind::Text, 0, 10);
    run_node(&mut map, "n2", NodeKind::Text, 21, 9);

    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 0,
    };
    let stored: TypstLocation = Location::Word {
        reference: TypstWordRef {
            label: "anno.A1B2".into(),
        },
    };
    let shown = crate::resolve::project(&ctx, &stored).expect("projects");
    let Location::Word { reference } = &shown else {
        panic!("wrong variant")
    };
    // The word sits at the end of the first run.
    assert_eq!(reference.node, "n1");
    assert_eq!(reference.end, 10);
    assert_eq!(reference.beg, 4);
    assert_eq!(reference.w.as_deref(), Some("monoid"));

    // And back again: the same anchor, and nothing new written.
    let back = crate::resolve::resolve(&ctx, &shown).expect("resolves");
    assert_eq!(back.location, stored);
    assert!(back.edits.is_empty());
}

#[test]
fn a_drawing_projects_to_the_element_it_follows() {
    use crate::render_map::*;
    let text = "#figure(image(\"x.png\"))<anno.D001>\n";
    let source = typst_syntax::Source::detached(text.to_owned());
    let mut map = RenderMap {
        render: "r1".into(),
        files: vec![FileEntry {
            path: "/docs/x.typ".into(),
            hash: "h".into(),
        }],
        nodes: Default::default(),
    };
    // The drawing's range ends where the label begins.
    map.nodes.insert(
        "n7".into(),
        NodeEntry {
            kind: NodeKind::Svg,
            range: Some(SrcRange {
                file: 0,
                start: 0,
                end: 23,
            }),
            segments: vec![],
        },
    );
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 0,
    };
    let shown = crate::resolve::project(
        &ctx,
        &Location::Svg {
            reference: TypstNodeRef {
                label: "anno.D001".into(),
            },
        },
    )
    .expect("projects");
    let Location::Svg { reference } = &shown else {
        panic!("wrong variant")
    };
    assert_eq!(reference.node, "n7");
}

#[test]
fn an_anchor_the_document_no_longer_has_does_not_project() {
    let text = "Nothing anchored here.\n";
    let (source, map) = fixture(text);
    let ctx = crate::resolve::Context {
        map: &map,
        was: text,
        source: &source,
        seed: 0,
    };
    let out = crate::resolve::project(
        &ctx,
        &Location::Word {
            reference: TypstWordRef {
                label: "anno.A1B2".into(),
            },
        },
    );
    assert_eq!(out, Err(crate::resolve::Failure::NoSuchNode("anno.A1B2".into())));
}
