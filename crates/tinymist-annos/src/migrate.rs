//! Translating positions from one version of a file to another.
//!
//! A browser reports a position against the rendering it is showing, which was
//! made from the file as it was at the time. By the time the position arrives,
//! the file may have changed — the user typed, or an agent rewrote a block. The
//! position must therefore be translated before it is used.
//!
//! The translation is derived by comparing the two texts. Regions that are
//! unchanged translate exactly; a position inside a region that changed has no
//! counterpart and is reported as lost. A lost position is not an error: it
//! means the text the position referred to is no longer there, which the caller
//! must handle rather than paper over.

use std::ops::Range;

/// What became of a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    /// The position is at this offset in the new text.
    At(usize),
    /// The text this position was in has changed; there is no counterpart.
    Lost,
}

impl Shift {
    /// The new offset, if there is one.
    pub fn offset(self) -> Option<usize> {
        match self {
            Self::At(offset) => Some(offset),
            Self::Lost => None,
        }
    }
}

/// A translation from offsets in one version of a file to offsets in another.
///
/// Built from the parts the two versions have in common. Each part is recorded
/// as the range it occupies in the old text and the offset it starts at in the
/// new one; everything between the parts changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rebase {
    kept: Vec<(Range<usize>, usize)>,
}

/// The largest changed region for which matching lines are still searched. A
/// larger region is treated as wholly changed, which costs recall on very large
/// edits and bounds the work at O(n·m) on the region rather than on the file.
const MAX_SEARCH_LINES: usize = 2000;

impl Rebase {
    /// The translation between two versions of a file.
    pub fn between(old: &str, new: &str) -> Self {
        if old == new {
            return Self {
                kept: vec![(0..old.len(), 0)],
            };
        }

        let old_lines = lines_of(old);
        let new_lines = lines_of(new);

        // Most edits touch one place, so the shared head and tail are usually
        // nearly the whole file. Matching them first bounds the work below.
        let mut head = 0;
        while head < old_lines.len()
            && head < new_lines.len()
            && text(old, &old_lines[head]) == text(new, &new_lines[head])
        {
            head += 1;
        }
        let mut tail = 0;
        while tail < old_lines.len() - head
            && tail < new_lines.len() - head
            && text(old, &old_lines[old_lines.len() - 1 - tail])
                == text(new, &new_lines[new_lines.len() - 1 - tail])
        {
            tail += 1;
        }

        let mut kept: Vec<(Range<usize>, usize)> = Vec::new();
        for at in 0..head {
            kept.push((old_lines[at].clone(), new_lines[at].start));
        }

        // The middle: lines that are common to both, in order. Skipped when the
        // region is large, in which case the middle counts as changed.
        let old_mid = &old_lines[head..old_lines.len() - tail];
        let new_mid = &new_lines[head..new_lines.len() - tail];
        if old_mid.len() <= MAX_SEARCH_LINES && new_mid.len() <= MAX_SEARCH_LINES {
            for (old_at, new_at) in common_lines(old, old_mid, new, new_mid) {
                kept.push((old_mid[old_at].clone(), new_mid[new_at].start));
            }
        }

        for at in 0..tail {
            let old_line = &old_lines[old_lines.len() - 1 - at];
            let new_line = &new_lines[new_lines.len() - 1 - at];
            kept.push((old_line.clone(), new_line.start));
        }

        kept.sort_by_key(|(range, _)| range.start);
        Self { kept }
    }

    /// Where a position in the old text sits in the new one.
    ///
    /// The last kept part beginning at or before the position is the one that
    /// applies, so a position on the boundary between two kept parts belongs to
    /// the later one. A position at the end of the last kept part still
    /// translates, which is what makes the end of a line or of a file a
    /// position rather than a failure.
    pub fn at(&self, offset: usize) -> Shift {
        let idx = self.kept.partition_point(|(range, _)| range.start <= offset);
        if idx == 0 {
            return Shift::Lost;
        }
        let (range, start) = &self.kept[idx - 1];
        if offset <= range.end {
            Shift::At(start + (offset - range.start))
        } else {
            Shift::Lost
        }
    }

    /// Whether the two versions were the same.
    pub fn is_identity(&self) -> bool {
        self.kept.len() == 1 && self.kept[0].1 == 0 && self.kept[0].0.start == 0
    }
}

/// The lines of a text, as ranges, including the newline that ends each.
fn lines_of(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (at, ch) in text.char_indices() {
        if ch == '\n' {
            lines.push(start..at + 1);
            start = at + 1;
        }
    }
    lines.push(start..text.len());
    lines
}

fn text<'a>(whole: &'a str, line: &Range<usize>) -> &'a str {
    &whole[line.clone()]
}

/// Pairs of matching lines, in order, by longest common subsequence.
fn common_lines(
    old: &str,
    old_lines: &[Range<usize>],
    new: &str,
    new_lines: &[Range<usize>],
) -> Vec<(usize, usize)> {
    let (rows, cols) = (old_lines.len(), new_lines.len());
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    // Lengths of the longest common subsequence of every suffix pair.
    let mut table = vec![0usize; (rows + 1) * (cols + 1)];
    let index = |row: usize, col: usize| row * (cols + 1) + col;
    for row in (0..rows).rev() {
        for col in (0..cols).rev() {
            table[index(row, col)] =
                if text(old, &old_lines[row]) == text(new, &new_lines[col]) {
                    table[index(row + 1, col + 1)] + 1
                } else {
                    table[index(row + 1, col)].max(table[index(row, col + 1)])
                };
        }
    }

    let mut pairs = Vec::new();
    let (mut row, mut col) = (0, 0);
    while row < rows && col < cols {
        if text(old, &old_lines[row]) == text(new, &new_lines[col]) {
            pairs.push((row, col));
            row += 1;
            col += 1;
        } else if table[index(row + 1, col)] >= table[index(row, col + 1)] {
            row += 1;
        } else {
            col += 1;
        }
    }
    pairs
}
