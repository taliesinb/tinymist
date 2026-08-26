//! Names for elements that survive the document being built again.
//!
//! A served document has a server to re-find things with: it keeps the map of
//! every rendering it has handed out, and translates an old position into the
//! current one. A published page has none of that. It is built once, uploaded,
//! and read by somebody whose annotation was stored against a name several
//! rebuilds ago. The name in the page is the only handle, so the name has to
//! carry its own meaning.
//!
//! A counter cannot: `n692` says only how many elements precede this one, so
//! inserting a paragraph renames everything below it. A hash of the text cannot
//! either, for the opposite reason — a cryptographic hash is built to avalanche,
//! so a paragraph that lost a typo gets a name as unrelated as a paragraph from
//! another page.
//!
//! So a name here is not one digest but several, each of a different thing
//! about the element, packed into one 128-bit value. An edit that changes one
//! facet leaves the others standing: rewrite a paragraph and its neighbours
//! still vouch for it; replace the paragraph above it and its own text still
//! does. The fields are witnesses, and they are independent, which is the point.
//!
//! Two rules make the difference between this working and not:
//!
//! - **Compare the fields, not the number.** An avalanched field either matches
//!   or contributes about half its bits at random, so the Hamming distance
//!   between two whole names cannot tell "the previous block was replaced" from
//!   "a few bits of the semantic digest drifted". Both sides of a comparison
//!   are ours, so they are unpacked and each field is compared on its own terms.
//! - **A neighbour field digests the neighbour's content, never its name.**
//!   Names that quote other names are mutually recursive: one edit perturbs its
//!   neighbours, whose new names perturb theirs, and the whole page shifts.
//!   Depth one, always.

use std::fmt;

/// How many bits each field is given, in the order they are packed.
const LITERAL_BITS: u32 = 32;
const SEMANTIC_BITS: u32 = 48;
const NEIGHBOUR_BITS: u32 = 16;
const HEADING_BITS: u32 = 8;
const SHAPE_BITS: u32 = 8;

/// The length of the text window a shingle covers.
///
/// Characters, not words. A twelve-word paragraph has eleven word shingles,
/// too few to decide forty-eight bit positions, and the digest comes out noisy;
/// the same paragraph has around two hundred character four-grams.
const SHINGLE: usize = 4;

/// What each field is worth when two names are compared.
///
/// The element's own text is most of the evidence; its neighbours are the rest;
/// its heading and its shape break ties. These are the weights the trial in this
/// module's tests was run at.
const WEIGHT_LITERAL: f32 = 30.0;
const WEIGHT_SEMANTIC: f32 = 40.0;
const WEIGHT_NEIGHBOUR: f32 = 14.0;
const WEIGHT_HEADING: f32 = 6.0;
const WEIGHT_SHAPE: f32 = 2.0;

/// Everything a name could be worth, which is what a score is read against.
pub const FULL: f32 =
    WEIGHT_LITERAL + WEIGHT_SEMANTIC + 2.0 * WEIGHT_NEIGHBOUR + WEIGHT_HEADING + WEIGHT_SHAPE;

/// How much of the full score a match must reach to be believed.
///
/// Set from what a rewrite costs, which the tests in this module print. A
/// paragraph keeping half its words scores 0.48 and is taken; one keeping a
/// quarter scores 0.34 and is not. Its neighbours, its heading and its shape
/// all agreeing, with nothing of the text left, comes to 0.34 as well — so
/// context alone is never an identification, which is deliberate. A slot
/// between the same two paragraphs holding entirely different words holds
/// different words, and a remark about the old ones does not belong to them.
pub const BAR: f32 = 0.45;

/// How far it must beat the next-best candidate.
///
/// The load-bearing test. A page of similar-looking table rows scores them all
/// alike, and what tells an edited paragraph from its neighbours is not how well
/// it scored but how much better it scored than anything else.
pub const MARGIN: f32 = 0.06;

/// What an element is, for the purpose of telling it from another one.
///
/// Every field is derived from something different, so that an edit which
/// changes one of them leaves the rest able to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    /// The element's own text, exactly: matches or does not.
    pub literal: u32,
    /// The same text by its character shingles: matches by degree, so a
    /// rewritten paragraph is still recognisably a rewrite of this one.
    pub semantic: u64,
    /// The block before it.
    pub previous: u16,
    /// The block after it.
    pub next: u16,
    /// The chain of headings above it. What "the parent" means in a document:
    /// the enclosing element itself is the body or a section wrapper for nearly
    /// every block on a page, and tells them apart not at all.
    pub heading: u8,
    /// Its kind and the tags of its children. Independent of its text, which is
    /// what its first child would otherwise have repeated.
    pub shape: u8,
}

/// What an element is called: its fields, written as one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(pub u128);

impl Fields {
    /// The fields of an element.
    ///
    /// `text` is what it says, `previous` and `next` what the blocks either
    /// side of it say, `heading` the headings above it joined into one string,
    /// and `shape` its kind followed by the tags of its children.
    pub fn of(text: &str, previous: &str, next: &str, heading: &str, shape: &str) -> Self {
        Self {
            literal: digest(Domain::Literal, &plain(text)) as u32,
            semantic: shingle_hash(&plain(text)),
            previous: digest(Domain::Previous, &plain(previous)) as u16,
            next: digest(Domain::Next, &plain(next)) as u16,
            heading: digest(Domain::Heading, &plain(heading)) as u8,
            shape: digest(Domain::Shape, shape) as u8,
        }
    }

    /// The fields as one number.
    pub fn name(&self) -> Name {
        let mut out: u128 = 0;
        let mut at = 0;
        for (value, bits) in [
            (self.literal as u128, LITERAL_BITS),
            (self.semantic as u128, SEMANTIC_BITS),
            (self.previous as u128, NEIGHBOUR_BITS),
            (self.next as u128, NEIGHBOUR_BITS),
            (self.heading as u128, HEADING_BITS),
            (self.shape as u128, SHAPE_BITS),
        ] {
            out |= (value & mask(bits)) << at;
            at += bits;
        }
        Name(out)
    }

    /// The fields a name was made of.
    pub fn from_name(name: Name) -> Self {
        let mut at = 0;
        let mut take = |bits: u32| {
            let value = (name.0 >> at) & mask(bits);
            at += bits;
            value
        };
        Self {
            literal: take(LITERAL_BITS) as u32,
            semantic: take(SEMANTIC_BITS) as u64,
            previous: take(NEIGHBOUR_BITS) as u16,
            next: take(NEIGHBOUR_BITS) as u16,
            heading: take(HEADING_BITS) as u8,
            shape: take(SHAPE_BITS) as u8,
        }
    }

    /// How much this element and that one agree, out of [`FULL`].
    ///
    /// Every field but the semantic one is a yes or a no. The semantic field is
    /// the only one that grades, and it grades from agreement rather than from
    /// distance: half the bits differing is what two unrelated texts give, so
    /// that is where its contribution reaches zero rather than halfway.
    pub fn agreement(&self, other: &Self) -> f32 {
        let yes = |same: bool, weight: f32| if same { weight } else { 0.0 };
        let differing = (self.semantic ^ other.semantic).count_ones() as f32;
        let alike = 1.0 - 2.0 * differing / SEMANTIC_BITS as f32;
        yes(self.literal == other.literal, WEIGHT_LITERAL)
            + WEIGHT_SEMANTIC * alike.max(0.0)
            + yes(self.previous == other.previous, WEIGHT_NEIGHBOUR)
            + yes(self.next == other.next, WEIGHT_NEIGHBOUR)
            + yes(self.heading == other.heading, WEIGHT_HEADING)
            + yes(self.shape == other.shape, WEIGHT_SHAPE)
    }

    /// Whether anything said about the text — its own or its neighbours' —
    /// matched outright.
    ///
    /// When this is false for every element of a page, the page was rewritten
    /// as a whole: a copyedit, a template change. That is a state worth
    /// recognising rather than a low score, since the semantic field is then
    /// the only witness left and what follows should ask more of it rather
    /// than refuse everything.
    ///
    /// The heading and the shape are left out. They agree between elements that
    /// have nothing to do with each other — every paragraph of a section shares
    /// a heading — so their agreeing says nothing about whether the page was
    /// rewritten.
    pub fn any_exact(&self, other: &Self) -> bool {
        self.literal == other.literal
            || self.previous == other.previous
            || self.next == other.next
    }
}

/// What became of an element that was named once and looked for later.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Found {
    /// It is this one, by this much of the full score.
    At {
        /// Where in the candidates it is.
        index: usize,
        /// How much of [`FULL`] the two agreed on.
        score: f32,
    },
    /// Nothing was convincing enough to say. The annotation belongs to the
    /// document now, which is what happens to one whose anchor is deleted.
    ///
    /// Refusing is the right answer more often than it looks. An element that
    /// was rewritten *and* moved has nothing left to recognise it by, and a
    /// deleted one is not there at all; a scheme that answers anyway attaches
    /// the remark to a sentence nobody wrote it about, which is worse than
    /// losing it.
    Nothing {
        /// The best score anything reached, for saying why.
        best: f32,
    },
}

/// Which of the candidates was named `looking_for`, if any of them was.
///
/// Every candidate is scored and the best one taken, but only if it clears
/// [`BAR`] and beats the runner-up by [`MARGIN`]. Both tests matter and the
/// second matters more: a high score shared with three other rows of the same
/// table is not an identification.
pub fn find(looking_for: &Fields, candidates: &[Fields]) -> Found {
    let mut best = (f32::MIN, usize::MAX);
    let mut second = f32::MIN;
    for (index, candidate) in candidates.iter().enumerate() {
        let score = looking_for.agreement(candidate);
        if score > best.0 {
            second = best.0;
            best = (score, index);
        } else if score > second {
            second = score;
        }
    }
    let (score, index) = best;
    if index == usize::MAX {
        return Found::Nothing { best: 0.0 };
    }
    let share = score / FULL;
    let margin = (score - second.max(0.0)) / FULL;
    if share < BAR || margin < MARGIN {
        return Found::Nothing { best: share };
    }
    Found::At { index, score }
}

impl fmt::Display for Name {
    /// Thirty-two hexadecimal digits, which is what goes in the page.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl std::str::FromStr for Name {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // Written by this crate, but read back from a page that anything may
        // have touched, so what is not a name is refused rather than trusted.
        if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{text} is not an element name"));
        }
        u128::from_str_radix(text, 16)
            .map(Name)
            .map_err(|err| err.to_string())
    }
}

/// The low `bits` bits.
fn mask(bits: u32) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

/// Text as it is compared: case folded, and every run of whitespace one space.
///
/// A rebuild rewraps lines and a copyedit changes case, and neither is a
/// different paragraph.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// Which field a digest is for.
///
/// The same text in two fields must not give the same bits, or a paragraph
/// would agree with itself for the wrong reason — its own text matching what
/// its neighbour's text was.
#[derive(Debug, Clone, Copy)]
enum Domain {
    Literal,
    Previous,
    Next,
    Heading,
    Shape,
    Shingle,
}

impl Domain {
    /// Where its digests start from.
    fn seed(self) -> u64 {
        // The FNV offset basis, moved along once per field.
        0xcbf2_9ce4_8422_2325u64.wrapping_add(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(self as u64))
    }
}

/// A digest of some bytes, avalanched.
///
/// FNV-1a, which is short and easy to write again in another language, followed
/// by the SplitMix64 finalizer, which is what makes every output bit depend on
/// every input bit. FNV alone does not, and a digest whose high bits barely move
/// would give a field that barely works.
fn digest(domain: Domain, text: &str) -> u64 {
    let mut hash = domain.seed();
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    mix(hash)
}

/// SplitMix64's finalizer.
fn mix(mut z: u64) -> u64 {
    z ^= z >> 30;
    z = z.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The digest that grades: a text's character shingles, summed by bit.
///
/// Every shingle votes on every bit position, and a bit ends up set if the votes
/// for it outweigh the votes against. Two texts sharing most of their shingles
/// therefore share most of their bits, which is the property the whole scheme
/// rests on and the one a cryptographic hash refuses to have.
fn shingle_hash(text: &str) -> u64 {
    let chars: Vec<char> = text.chars().collect();
    let mut votes = [0i32; SEMANTIC_BITS as usize];
    let mut count = 0;
    let mut cast = |window: &str| {
        let hash = digest(Domain::Shingle, window);
        for (bit, vote) in votes.iter_mut().enumerate() {
            *vote += if (hash >> bit) & 1 == 1 { 1 } else { -1 };
        }
        count += 1;
    };
    if chars.len() <= SHINGLE {
        // Too short to cut up. Its whole text is its one feature, which makes
        // the field nearly all-or-nothing — as it should be, since there is not
        // enough here to be partly the same.
        if !chars.is_empty() {
            cast(text);
        }
    } else {
        for window in chars.windows(SHINGLE) {
            cast(&window.iter().collect::<String>());
        }
    }
    if count == 0 {
        return 0;
    }
    let mut out = 0u64;
    for (bit, vote) in votes.iter().enumerate() {
        if *vote > 0 {
            out |= 1 << bit;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page: paragraphs in order, under headings.
    struct Page {
        blocks: Vec<String>,
    }

    impl Page {
        fn new(blocks: &[&str]) -> Self {
            Self {
                blocks: blocks.iter().map(|b| (*b).to_owned()).collect(),
            }
        }

        fn heading(&self, at: usize) -> String {
            format!("section {}", at / 3)
        }

        fn fields(&self, at: usize) -> Fields {
            Fields::of(
                &self.blocks[at],
                if at > 0 { &self.blocks[at - 1] } else { "" },
                self.blocks.get(at + 1).map(String::as_str).unwrap_or(""),
                &self.heading(at),
                "para",
            )
        }

        fn all(&self) -> Vec<Fields> {
            (0..self.blocks.len()).map(|at| self.fields(at)).collect()
        }
    }

    /// Nine paragraphs of ordinary prose, distinct but not wildly so.
    fn page() -> Page {
        Page::new(&[
            "The document carries only anchors: bare labels that move with the text they are written next to.",
            "Everything else lives in a sidecar file that says which anchor each annotation points at.",
            "A Typst element carries at most one label, so two remarks about one word must share an anchor.",
            "The board, with this row added. Every other entry is a machine trained by gradient descent.",
            "Scored under the board's own protocol: grammar draw zero, three seeds, twenty thousand examples.",
            "The medians are as published, and the column counts how many of an attempt's seeds passed.",
            "A capture is an image of what an annotation refers to, taken at a particular moment in time.",
            "It exists because the document is source: an agent asked about a plot cannot otherwise see it.",
            "The image itself is not in the sidecar; it is stored by hash and served from the capture store.",
        ])
    }

    /// Words that appear nowhere in the page and share nothing with each other,
    /// so that replacing text with them is a rewrite rather than a repetition.
    const OTHER: &[&str] = &[
        "quantum", "beige", "harbour", "trellis", "moth", "vinegar", "ledger",
        "orbit", "pumice", "wharf", "citrus", "gantry", "flint", "meadow",
        "sonar", "brocade", "juniper", "kiln", "plinth", "warbler",
    ];

    /// The same block, rewritten: the given share of its words kept, the rest
    /// replaced by words from elsewhere.
    fn reworded(block: &str, keep: f32) -> String {
        let words: Vec<&str> = block.split(' ').collect();
        let kept = (words.len() as f32 * keep).round() as usize;
        words
            .iter()
            .enumerate()
            .map(|(at, word)| {
                if at < kept {
                    (*word).to_owned()
                } else {
                    OTHER[at % OTHER.len()].to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_name_is_thirty_two_hex_digits_and_reads_back() {
        let fields = page().fields(3);
        let name = fields.name();
        let written = name.to_string();
        assert_eq!(written.len(), 32);
        assert!(written.bytes().all(|b| b.is_ascii_hexdigit()), "{written}");
        assert_eq!(written.parse::<Name>().unwrap(), name);
        assert_eq!(Fields::from_name(name), fields);
    }

    #[test]
    fn what_is_not_a_name_is_refused() {
        assert!("".parse::<Name>().is_err());
        assert!("n692".parse::<Name>().is_err());
        assert!("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".parse::<Name>().is_err());
    }

    #[test]
    fn an_unchanged_page_names_everything_the_same() {
        assert_eq!(page().all(), page().all());
    }

    /// The case the fields exist for: a paragraph rewritten where it stands.
    /// Its own text no longer matches, and its neighbours still vouch for it.
    #[test]
    fn a_rewritten_paragraph_is_still_found() {
        let was = page();
        let mut now = page();
        now.blocks[4] = reworded(&now.blocks[4], 0.5);
        match find(&was.fields(4), &now.all()) {
            Found::At { index, .. } => assert_eq!(index, 4),
            other => panic!("lost it: {other:?}"),
        }
    }

    /// The other way round: the paragraph is untouched and the one above it is
    /// gone.
    #[test]
    fn a_paragraph_whose_neighbour_changed_is_still_found() {
        let was = page();
        let mut now = page();
        now.blocks[3] = reworded(&now.blocks[3], 0.0);
        match find(&was.fields(4), &now.all()) {
            Found::At { index, .. } => assert_eq!(index, 4),
            other => panic!("lost it: {other:?}"),
        }
    }

    #[test]
    fn an_insertion_above_does_not_lose_what_is_below_it() {
        let was = page();
        let mut now = page();
        now.blocks.insert(1, "A paragraph that was not here before.".into());
        match find(&was.fields(6), &now.all()) {
            Found::At { index, .. } => assert_eq!(index, 7),
            other => panic!("lost it: {other:?}"),
        }
    }

    #[test]
    fn a_paragraph_moved_elsewhere_is_still_found() {
        let was = page();
        let mut now = page();
        let moved = now.blocks.remove(7);
        now.blocks.insert(1, moved);
        match find(&was.fields(7), &now.all()) {
            Found::At { index, .. } => assert_eq!(index, 1),
            other => panic!("lost it: {other:?}"),
        }
    }

    /// The property worth more than any of the recoveries above: when the
    /// element is gone, nothing is offered in its place.
    #[test]
    fn a_deleted_paragraph_matches_nothing() {
        let was = page();
        let mut now = page();
        now.blocks.remove(4);
        assert!(
            matches!(find(&was.fields(4), &now.all()), Found::Nothing { .. }),
            "something was offered for a paragraph that is gone"
        );
    }

    /// Rewritten *and* moved: there is nothing left that was true of it, and
    /// the answer is to say so.
    #[test]
    fn a_paragraph_rewritten_and_moved_matches_nothing() {
        let was = page();
        let mut now = page();
        let moved = reworded(&now.blocks.remove(4), 0.1);
        now.blocks.insert(0, moved);
        assert!(
            matches!(find(&was.fields(4), &now.all()), Found::Nothing { .. }),
            "something was offered for a paragraph with nothing left of it"
        );
    }

    /// A page of near-identical rows is what the margin is for: every candidate
    /// scores well, and scoring well is not an identification.
    #[test]
    fn one_of_several_alike_rows_is_not_guessed_at() {
        let rows = Page::new(&[
            "licensed resst, entmax content tables, seed 1",
            "licensed resst, entmax content tables, seed 2",
            "licensed resst, entmax content tables, seed 3",
            "licensed resst, entmax content tables, seed 4",
        ]);
        // The row that was annotated is deleted; the three left are its
        // siblings, and none of them is it.
        let mut now = Page::new(&[
            "licensed resst, entmax content tables, seed 1",
            "licensed resst, entmax content tables, seed 3",
            "licensed resst, entmax content tables, seed 4",
        ]);
        now.blocks.dedup();
        assert!(
            matches!(find(&rows.fields(1), &now.all()), Found::Nothing { .. }),
            "a deleted row was mistaken for one of its siblings"
        );
    }

    #[test]
    fn a_field_says_when_nothing_matched_outright() {
        let was = page();
        let mut now = page();
        // Every block edited a little: the signature of a copyedit, where no
        // exact field can agree and only the graded one is left.
        for block in &mut now.blocks {
            *block = format!("{block} And one more sentence.");
        }
        assert!(!was.fields(4).any_exact(&now.fields(4)));
        assert!(was.fields(4).any_exact(&page().fields(4)));
    }

    #[test]
    fn the_same_text_in_two_fields_gives_different_bits() {
        // Otherwise a paragraph would agree with itself for the wrong reason.
        let text = "The board, with this row added.";
        let a = Fields::of(text, "", "", "", "para");
        let b = Fields::of("", text, "", "", "para");
        assert_ne!(a.literal as u64, b.previous as u64);
    }

    #[test]
    fn whitespace_and_case_are_not_a_different_paragraph() {
        let one = Fields::of("The  board,\n  with this row added.", "", "", "", "para");
        let two = Fields::of("the board, with this row added.", "", "", "", "para");
        assert_eq!(one, two);
    }

    /// What each field is worth on a paragraph rewritten to a given degree,
    /// printed so the bar can be set from evidence rather than from taste.
    #[test]
    fn what_a_rewrite_costs() {
        let was = page();
        for keep in [1.0, 0.75, 0.5, 0.25, 0.0] {
            let mut now = page();
            now.blocks[4] = reworded(&now.blocks[4], keep);
            let (a, b) = (was.fields(4), now.fields(4));
            let differing = (a.semantic ^ b.semantic).count_ones();
            println!(
                "keep {keep:>4}  literal {}  semantic {differing:>2}/48  score {:.3}",
                a.literal == b.literal,
                a.agreement(&b) / FULL,
            );
        }
    }
}
