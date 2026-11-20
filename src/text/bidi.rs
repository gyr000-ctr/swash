/*!
Minimal bidirectional text resolution.

This module implements a deliberately small subset of the Unicode
Bidirectional Algorithm (UAX #9) that is sufficient for laying out mixed
left-to-right and right-to-left text:

* resolution of bidi classes to embedding levels (rules P1-P3, W1-W7,
  N1-N2, I1-I2 and L1)
* splitting text into unidirectional runs and computing their visual
  order (rule L2)
* a bidirectional mapping between logical and visual character indices
  so that shaped clusters can be mapped back to the source text

Explicit directional controls (embeddings, overrides and isolates) are
out of scope. Rather than silently misordering text that contains them,
resolution fails with [`BidiError::ExplicitControl`]. Characters with
the `BN` class (such as the soft hyphen) are treated as neutral.

All buffers are retained between calls to [`Bidi::resolve`], so after
warm up, resolution performs no heap allocation.
*/

use super::{BidiClass, Codepoint};
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::fmt;

use BidiClass::*;

/// Error produced by bidi resolution.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BidiError {
    /// The text contains an explicit directional control (an embedding,
    /// override, isolate or a matching terminator) which is not
    /// supported by this resolver.
    ExplicitControl {
        /// Index (in characters) of the control character.
        index: usize,
        /// Bidi class of the control character.
        class: BidiClass,
    },
}

impl fmt::Display for BidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitControl { index, class } => write!(
                f,
                "unsupported explicit bidi control {class:?} at character {index}"
            ),
        }
    }
}

impl core::error::Error for BidiError {}

/// A maximal sequence of characters with the same embedding level.
///
/// Runs produced by [`Bidi::runs`] are ordered visually. Each run covers
/// a contiguous range of logical character indices and should be shaped
/// in the direction implied by its level.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Run {
    /// Start of the run in logical character indices.
    pub start: usize,
    /// End of the run in logical character indices.
    pub end: usize,
    /// Resolved embedding level of the run.
    pub level: u8,
}

impl Run {
    /// Returns the length of the run in characters.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Returns true if the run is empty.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Returns true if the run is in right-to-left order.
    pub fn is_rtl(&self) -> bool {
        self.level & 1 != 0
    }
}

/// Reusable context for bidi resolution.
///
/// The context owns all buffers used during resolution. They are
/// cleared, but not deallocated, by each call to [`resolve`](Self::resolve)
/// so that repeated resolution of similarly sized text performs no heap
/// allocation.
#[derive(Clone, Default)]
pub struct Bidi {
    /// Bidi class of each character (with `BN` mapped to `ON`).
    classes: Vec<BidiClass>,
    /// Working copy of the classes, mutated by the resolution rules.
    work: Vec<BidiClass>,
    /// Resolved embedding level of each character.
    levels: Vec<u8>,
    /// Unidirectional runs in visual order.
    runs: Vec<Run>,
    /// Logical character index for each visual position.
    visual_to_logical: Vec<u32>,
    /// Visual position for each logical character index.
    logical_to_visual: Vec<u32>,
    /// Embedding level of the first paragraph.
    paragraph_level: u8,
}

impl Bidi {
    /// Creates a new bidi resolution context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves the specified text.
    ///
    /// The text is split into paragraphs at `B` (paragraph separator)
    /// characters and each paragraph is resolved independently. Returns
    /// an error if the text contains explicit directional controls.
    ///
    /// On success, the results are available through the various
    /// accessor methods until the next call to this function.
    pub fn resolve<I>(&mut self, text: I) -> Result<(), BidiError>
    where
        I: IntoIterator,
        I::Item: Borrow<char>,
    {
        self.classes.clear();
        for (index, ch) in text.into_iter().enumerate() {
            let class = ch.borrow().bidi_class();
            match class {
                LRE | RLE | LRO | RLO | PDF | LRI | RLI | FSI | PDI => {
                    return Err(BidiError::ExplicitControl { index, class });
                }
                BN => self.classes.push(ON),
                _ => self.classes.push(class),
            }
        }
        let n = self.classes.len();
        self.work.clear();
        self.work.extend_from_slice(&self.classes);
        self.levels.clear();
        self.levels.resize(n, 0);
        self.runs.clear();
        self.visual_to_logical.clear();
        self.logical_to_visual.clear();
        self.paragraph_level = 0;
        let mut start = 0;
        let mut first = true;
        for i in 0..=n {
            if i == n || self.work[i] == B {
                let content_end = i;
                let para_end = if i < n { i + 1 } else { i };
                if start != para_end {
                    self.resolve_paragraph(start, content_end, para_end, first);
                    first = false;
                }
                start = i + 1;
            }
        }
        self.logical_to_visual.resize(n, 0);
        for (visual, &logical) in self.visual_to_logical.iter().enumerate() {
            self.logical_to_visual[logical as usize] = visual as u32;
        }
        Ok(())
    }

    /// Returns the embedding level of the first paragraph.
    pub fn paragraph_level(&self) -> u8 {
        self.paragraph_level
    }

    /// Returns true if the direction of the first paragraph is
    /// right-to-left.
    pub fn is_rtl(&self) -> bool {
        self.paragraph_level & 1 != 0
    }

    /// Returns the number of characters in the resolved text.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Returns true if the resolved text is empty.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Returns the bidi class of each character in logical order.
    pub fn classes(&self) -> &[BidiClass] {
        &self.classes
    }

    /// Returns the resolved embedding level of each character in logical
    /// order.
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// Returns the unidirectional runs in visual order.
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Returns the logical character index for each visual position.
    pub fn visual_to_logical(&self) -> &[u32] {
        &self.visual_to_logical
    }

    /// Returns the visual position for each logical character index.
    ///
    /// A cluster covering the logical characters `start..end` occupies a
    /// contiguous visual span: the minimum through the maximum of the
    /// entries in `start..end` of this map.
    pub fn logical_to_visual(&self) -> &[u32] {
        &self.logical_to_visual
    }

    /// Resolves a single paragraph. `start..end` is the content of the
    /// paragraph and `para_end` is `end + 1` when the paragraph is
    /// terminated by a `B` character (which belongs to the paragraph).
    fn resolve_paragraph(&mut self, start: usize, end: usize, para_end: usize, first: bool) {
        // P2/P3: the paragraph level is determined by the first strong
        // character, defaulting to left-to-right.
        let mut para = 0u8;
        for &class in &self.work[start..end] {
            match class {
                L => break,
                R | AL => {
                    para = 1;
                    break;
                }
                _ => {}
            }
        }
        if first {
            self.paragraph_level = para;
        }
        let sor = if para == 0 { L } else { R };
        let c = &mut self.work;
        // W1: non-spacing marks take the type of the preceding character
        // (or the start-of-run type).
        let mut prev = sor;
        for class in &mut c[start..end] {
            if *class == NSM {
                *class = prev;
            } else {
                prev = *class;
            }
        }
        // W2: European numbers preceded by an Arabic strong character
        // become Arabic numbers.
        let mut last_strong = sor;
        for class in &mut c[start..end] {
            match *class {
                L | R | AL => last_strong = *class,
                EN if last_strong == AL => *class = AN,
                _ => {}
            }
        }
        // W3: Arabic letters become right-to-left.
        for class in &mut c[start..end] {
            if *class == AL {
                *class = R;
            }
        }
        // W4: a single separator between two numbers of the same type
        // takes that type.
        if end - start >= 3 {
            for i in start + 1..end - 1 {
                let (a, b) = (c[i - 1], c[i + 1]);
                match c[i] {
                    ES if a == EN && b == EN => c[i] = EN,
                    CS if a == b && (a == EN || a == AN) => c[i] = a,
                    _ => {}
                }
            }
        }
        // W5: sequences of number terminators adjacent to a European
        // number become European numbers.
        let mut i = start;
        while i < end {
            if c[i] == ET {
                let mut j = i + 1;
                while j < end && c[j] == ET {
                    j += 1;
                }
                if (i > start && c[i - 1] == EN) || (j < end && c[j] == EN) {
                    for class in &mut c[i..j] {
                        *class = EN;
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
        // W6: remaining separators and terminators become neutral.
        for class in &mut c[start..end] {
            if matches!(*class, ES | CS | ET) {
                *class = ON;
            }
        }
        // W7: European numbers become L when the preceding strong type
        // is L.
        let mut last_strong = sor;
        for class in &mut c[start..end] {
            match *class {
                L => last_strong = L,
                R => last_strong = R,
                EN if last_strong == L => *class = L,
                _ => {}
            }
        }
        // N1/N2: sequences of neutrals take the surrounding strong type
        // when it is the same on both sides, or the embedding direction
        // otherwise.
        let strong = |class: BidiClass| match class {
            EN | AN | R => R,
            _ => L,
        };
        let neutral = |class: BidiClass| matches!(class, ON | WS | S);
        let mut prev_strong = sor;
        let mut i = start;
        while i < end {
            if neutral(c[i]) {
                let mut j = i + 1;
                while j < end && neutral(c[j]) {
                    j += 1;
                }
                let next_strong = if j < end { strong(c[j]) } else { sor };
                let resolved = if prev_strong == next_strong {
                    next_strong
                } else {
                    sor
                };
                for class in &mut c[i..j] {
                    *class = resolved;
                }
                i = j;
            } else {
                prev_strong = strong(c[i]);
                i += 1;
            }
        }
        // I1/I2: implicit levels.
        for (level, &class) in self.levels[start..end].iter_mut().zip(&c[start..end]) {
            *level = if para & 1 == 0 {
                match class {
                    R => para + 1,
                    EN | AN => para + 2,
                    _ => para,
                }
            } else {
                match class {
                    L | EN | AN => para + 1,
                    _ => para,
                }
            };
        }
        // L1: the paragraph separator, segment separators and adjacent
        // or trailing whitespace take the paragraph level. The original
        // classes are used here because the working classes have been
        // resolved away.
        if para_end > end {
            self.levels[end] = para;
        }
        let mut k = end;
        while k > start && matches!(self.classes[k - 1], WS | S) {
            k -= 1;
            self.levels[k] = para;
        }
        for i in start..end {
            if self.classes[i] == S {
                self.levels[i] = para;
                let mut k = i;
                while k > start && self.classes[k - 1] == WS {
                    k -= 1;
                    self.levels[k] = para;
                }
            }
        }
        // L2: compute the visual order for this paragraph by reversing
        // contiguous sequences of characters at or above each level,
        // from the maximum level down to the lowest odd level.
        let base = self.visual_to_logical.len();
        let len = para_end - start;
        self.visual_to_logical.extend(start as u32..para_end as u32);
        let mut max_level = 0u8;
        let mut min_odd = u8::MAX;
        for &level in &self.levels[start..para_end] {
            max_level = max_level.max(level);
            if level & 1 == 1 {
                min_odd = min_odd.min(level);
            }
        }
        if min_odd != u8::MAX {
            let mut level = max_level;
            loop {
                let mut i = 0;
                while i < len {
                    if self.levels[start + i] >= level {
                        let mut j = i + 1;
                        while j < len && self.levels[start + j] >= level {
                            j += 1;
                        }
                        self.visual_to_logical[base + i..base + j].reverse();
                        i = j;
                    } else {
                        i += 1;
                    }
                }
                if level == min_odd {
                    break;
                }
                level -= 1;
            }
        }
        // Split the visually ordered characters into runs of equal
        // level. Each run maps back to a contiguous logical range.
        let mut i = 0;
        while i < len {
            let first = self.visual_to_logical[base + i];
            let level = self.levels[first as usize];
            let mut lo = first;
            let mut hi = first;
            let mut j = i + 1;
            while j < len {
                let index = self.visual_to_logical[base + j];
                if self.levels[index as usize] != level {
                    break;
                }
                lo = lo.min(index);
                hi = hi.max(index);
                j += 1;
            }
            self.runs.push(Run {
                start: lo as usize,
                end: hi as usize + 1,
                level,
            });
            i = j;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use std::alloc::{GlobalAlloc, Layout, System};

    thread_local! {
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    /// Global allocator that counts allocations on the current thread so
    /// that the zero-allocation guarantee can be tested.
    struct CountingAlloc;

    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCS.with(|count| count.set(count.get() + 1));
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAlloc = CountingAlloc;

    fn resolve(text: &str) -> Bidi {
        let mut bidi = Bidi::new();
        bidi.resolve(text.chars()).unwrap();
        bidi
    }

    #[test]
    fn bidi_ltr_is_identity() {
        // Pure LTR text must pass through untouched: all levels zero, a
        // single run and identity mappings, so glyph sequences and
        // positions are byte-identical to the unresolved text.
        for text in [
            "Hello, world!",
            "The price is $12.50 (approx).",
            "abc 123 xyz",
            "Résumé, naïve, façade",
            "日本語のテキスト",
            "a\u{AD}b", // soft hyphen (BN) is treated as neutral
        ] {
            let bidi = resolve(text);
            let n = text.chars().count();
            assert_eq!(bidi.paragraph_level(), 0);
            assert_eq!(bidi.len(), n);
            assert!(bidi.levels().iter().all(|&level| level == 0));
            assert_eq!(bidi.runs(), &[Run { start: 0, end: n, level: 0 }]);
            let identity: Vec<u32> = (0..n as u32).collect();
            assert_eq!(bidi.visual_to_logical(), &identity[..]);
            assert_eq!(bidi.logical_to_visual(), &identity[..]);
        }
    }

    #[test]
    fn bidi_rtl_run_in_ltr() {
        // "abc אבג def"
        let bidi = resolve("abc \u{5D0}\u{5D1}\u{5D2} def");
        assert_eq!(bidi.paragraph_level(), 0);
        assert_eq!(bidi.levels(), &[0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0]);
        assert_eq!(bidi.visual_to_logical(), &[0, 1, 2, 3, 6, 5, 4, 7, 8, 9, 10]);
        assert_eq!(
            bidi.runs(),
            &[
                Run { start: 0, end: 4, level: 0 },
                Run { start: 4, end: 7, level: 1 },
                Run { start: 7, end: 11, level: 0 },
            ]
        );
    }

    #[test]
    fn bidi_rtl_paragraph() {
        // "אבג def": first strong is RTL, so the paragraph is RTL and the
        // LTR word is raised to level 2.
        let bidi = resolve("\u{5D0}\u{5D1}\u{5D2} def");
        assert_eq!(bidi.paragraph_level(), 1);
        assert!(bidi.is_rtl());
        assert_eq!(bidi.levels(), &[1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(bidi.visual_to_logical(), &[4, 5, 6, 3, 2, 1, 0]);
        assert_eq!(
            bidi.runs(),
            &[
                Run { start: 4, end: 7, level: 2 },
                Run { start: 0, end: 4, level: 1 },
            ]
        );
    }

    #[test]
    fn bidi_rtl_paragraph_with_digits() {
        // "אבג 123": European digits after a RTL strong stay EN and are
        // raised to level 2, keeping their internal LTR order.
        let bidi = resolve("\u{5D0}\u{5D1}\u{5D2} 123");
        assert_eq!(bidi.levels(), &[1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(bidi.visual_to_logical(), &[4, 5, 6, 3, 2, 1, 0]);
    }

    #[test]
    fn bidi_arabic_numbers() {
        // "abc ١٢": Arabic-Indic digits (AN) in a LTR paragraph get
        // level 2; nothing is reordered.
        let bidi = resolve("abc \u{661}\u{662}");
        assert_eq!(bidi.levels(), &[0, 0, 0, 0, 2, 2]);
        assert_eq!(bidi.visual_to_logical(), &[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            bidi.runs(),
            &[Run { start: 0, end: 4, level: 0 }, Run { start: 4, end: 6, level: 2 }]
        );
    }

    #[test]
    fn bidi_arabic_letter_european_digit() {
        // "ا1": W2 turns the European digit after AL into AN (visible
        // in the resolved levels; `classes` keeps the original class).
        let bidi = resolve("\u{627}1");
        assert_eq!(bidi.paragraph_level(), 1);
        assert_eq!(bidi.classes()[1], EN);
        assert_eq!(bidi.levels(), &[1, 2]);
        assert_eq!(bidi.visual_to_logical(), &[1, 0]);
        assert_eq!(
            bidi.runs(),
            &[Run { start: 1, end: 2, level: 2 }, Run { start: 0, end: 1, level: 1 }]
        );
    }

    #[test]
    fn bidi_number_separators() {
        // "a 1,2 b": the common separator joins the numbers (W4), then
        // W7 demotes everything to L in a LTR paragraph.
        let bidi = resolve("a 1,2 b");
        assert!(bidi.levels().iter().all(|&level| level == 0));
        assert_eq!(bidi.visual_to_logical(), &[0, 1, 2, 3, 4, 5, 6]);
        // "א 1.5": the European separator joins the numbers (W4) which
        // stay EN after a RTL strong and move to level 2 as a unit.
        let bidi = resolve("\u{5D0} 1.5");
        assert_eq!(bidi.levels(), &[1, 1, 2, 2, 2]);
        assert_eq!(bidi.visual_to_logical(), &[2, 3, 4, 1, 0]);
        assert_eq!(
            bidi.runs(),
            &[Run { start: 2, end: 5, level: 2 }, Run { start: 0, end: 2, level: 1 }]
        );
    }

    #[test]
    fn bidi_neutral_at_boundaries() {
        // "אבג!": trailing neutral takes the surrounding RTL type.
        let bidi = resolve("\u{5D0}\u{5D1}\u{5D2}!");
        assert_eq!(bidi.levels(), &[1, 1, 1, 1]);
        assert_eq!(bidi.visual_to_logical(), &[3, 2, 1, 0]);
    }

    #[test]
    fn bidi_paragraph_separator() {
        // "אב\nab": paragraphs are resolved independently; the separator
        // takes the level of its paragraph (L1).
        let bidi = resolve("\u{5D0}\u{5D1}\nab");
        assert_eq!(bidi.levels(), &[1, 1, 1, 0, 0]);
        assert_eq!(bidi.visual_to_logical(), &[2, 1, 0, 3, 4]);
        assert_eq!(
            bidi.runs(),
            &[Run { start: 0, end: 3, level: 1 }, Run { start: 3, end: 5, level: 0 }]
        );
    }

    #[test]
    fn bidi_segment_separator_takes_paragraph_level() {
        // "aא\u{1F}א": the segment separator is reset to the paragraph
        // level (L1) even though it sits between two RTL characters.
        let bidi = resolve("a\u{5D0}\u{1F}\u{5D0}");
        assert_eq!(bidi.classes()[2], S);
        assert_eq!(bidi.levels(), &[0, 1, 0, 1]);
    }

    #[test]
    fn bidi_explicit_controls_are_rejected() {
        for (text, index, class) in [
            ("abc\u{202A}def", 3, LRE),
            ("abc\u{202B}def", 3, RLE),
            ("abc\u{202C}def", 3, PDF),
            ("abc\u{202D}def", 3, LRO),
            ("abc\u{202E}def", 3, RLO),
            ("abc\u{2066}def", 3, LRI),
            ("abc\u{2067}def", 3, RLI),
            ("abc\u{2068}def", 3, FSI),
            ("abc\u{2069}def", 3, PDI),
            ("\u{202E}", 0, RLO),
        ] {
            let mut bidi = Bidi::new();
            assert_eq!(
                bidi.resolve(text.chars()),
                Err(BidiError::ExplicitControl { index, class }),
                "text: {text:?}"
            );
        }
    }

    #[test]
    fn bidi_cluster_mapping() {
        // Clusters are contiguous logical ranges and map to contiguous
        // visual spans. "abc אבג def"
        let bidi = resolve("abc \u{5D0}\u{5D1}\u{5D2} def");
        let l2v = bidi.logical_to_visual();
        let v2l = bidi.visual_to_logical();
        // The maps are inverse permutations.
        for (logical, &visual) in l2v.iter().enumerate() {
            assert_eq!(v2l[visual as usize], logical as u32);
        }
        // The Hebrew cluster (logical 4..7) occupies visual span 4..7.
        let (lo, hi) = (4..7)
            .map(|i| l2v[i])
            .fold((u32::MAX, 0), |(lo, hi), v| (lo.min(v), hi.max(v)));
        assert_eq!((lo, hi), (4, 6));
        let mut mapped: Vec<u32> = v2l[lo as usize..=hi as usize].to_vec();
        mapped.sort_unstable();
        assert_eq!(mapped, &[4, 5, 6]);
    }

    #[test]
    fn bidi_empty_text() {
        let bidi = resolve("");
        assert_eq!(bidi.paragraph_level(), 0);
        assert_eq!(bidi.len(), 0);
        assert!(bidi.is_empty());
        assert!(bidi.runs().is_empty());
        assert!(bidi.visual_to_logical().is_empty());
        assert!(bidi.logical_to_visual().is_empty());
    }

    #[test]
    fn bidi_no_allocation_after_warmup() {
        let mut bidi = Bidi::new();
        let mixed = "abc \u{5D0}\u{5D1}\u{5D2} def 123 \u{627}\u{628}!\nxyz";
        // Warm up: grow the buffers.
        bidi.resolve(mixed.chars()).unwrap();
        bidi.resolve("short".chars()).unwrap();
        ALLOCS.with(|count| count.set(0));
        for _ in 0..16 {
            bidi.resolve(mixed.chars()).unwrap();
            bidi.resolve("short".chars()).unwrap();
        }
        assert_eq!(ALLOCS.with(Cell::get), 0, "resolution allocated on the heap");
    }
}
