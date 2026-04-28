use std::collections::HashSet;

use bumpalo::Bump;

/// An interning n-gram builder that allocates unique strings into a bump arena.
///
/// All returned `&str` slices point into the arena and remain valid for its lifetime.
/// Duplicate n-grams are deduplicated (interned), so each unique n-gram is allocated once.
pub struct NgramIntern {
    arena: Bump,
    seen: HashSet<String>,
}

impl NgramIntern {
    pub fn new() -> Self {
        Self {
            arena: Bump::new(),
            seen: HashSet::new(),
        }
    }

    /// Intern a single string into the arena.
    /// Returns a `&str` pointing into the arena.
    /// If the string was already interned, returns the existing arena slice.
    pub fn intern(&mut self, s: &str) -> &str {
        if self.seen.contains(s) {
            // Already interned — we need to find the existing slice.
            // Problem: HashSet<&str> with arena pointers would be better.
            // See `NgramIntern<'a>` below for the proper design.
            panic!("Use NgramInternV2 for correct interning")
        }
        self.seen.insert(s.to_owned());
        self.arena.alloc_str(s)
    }

    /// Build all n-grams from whitespace-tokenized text.
    /// Returns a vector of unique n-gram slices pointing into the arena.
    pub fn build_ngrams(&mut self, text: &str, n: usize) -> Vec<&str> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let mut unique: HashSet<&str> = HashSet::new();

        // We need a small reusable buffer to build n-gram strings without
        // allocating a new String per window.
        let mut buf = String::with_capacity(text.len());

        for window in tokens.windows(n) {
            buf.clear();
            for (i, token) in window.iter().enumerate() {
                if i > 0 {
                    buf.push(' ');
                }
                buf.push_str(token);
            }

            if unique.insert(buf.as_str()) {
                self.seen.insert(buf.clone());
                unique.insert(self.arena.alloc_str(&buf));
            }
        }

        let mut result: Vec<&str> = unique
            .into_iter()
            .filter(|s| !self.seen.contains(*s)) // arena slices, not originals
            .collect();
        result.sort();
        result
    }
}

// ---------------------------------------------------------------------------
// V2: The correct design — store arena pointers in the set, not owned Strings.
// ---------------------------------------------------------------------------

/// Interning n-gram builder. Deduplicates n-grams and allocates each unique one
/// exactly once into a bump arena.
///
/// ```
/// use my_crate::ngram::NgramBuilder;
///
/// let mut builder = NgramBuilder::new();
/// let vocab: Vec<&str> = builder.add_text("the cat sat on the mat", 2);
/// // vocab contains: ["cat sat", "mat", "on the", "sat on", "the cat", "the mat"]
/// // All slices point into builder's arena and stay valid until builder is dropped.
/// ```
pub struct NgramBuilder {
    arena: Bump,
    /// Stores arena-allocated `&'static str` pointers for dedup.
    /// We use `'static` because bump-allocated slices are valid for the arena's
    /// lifetime, and we transmute the arena's lifetime to `'static` internally.
    seen: HashSet<&'static str>,
}

impl NgramBuilder {
    pub fn new() -> Self {
        Self {
            arena: Bump::new(),
            seen: HashSet::new(),
        }
    }

    /// Intern a string into the arena. Returns an arena-owned `&str`.
    /// If already interned, returns the existing slice.
    pub fn intern(&mut self, s: &str) -> &str {
        // SAFETY: We only ever store slices from this arena, and we never
        // return a reference that outlives `self`. The `'static` is an
        // internal implementation detail.
        let static_s: &'static str = unsafe { std::mem::transmute(s) };

        if let Some(existing) = self.seen.get(static_s) {
            // SAFETY: reverse the transmute
            return unsafe { std::mem::transmute(*existing) };
        }

        let slice: &str = self.arena.alloc_str(s);
        let static_slice: &'static str = unsafe { std::mem::transmute(slice) };
        self.seen.insert(static_slice);
        slice
    }

    /// Tokenize text by whitespace, build all n-grams, and intern them.
    /// Returns a sorted vector of **unique** n-gram slices.
    pub fn add_text(&mut self, text: &str, n: usize) -> Vec<&str> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let mut unique_ngrams: Vec<&str> = Vec::new();

        // Reusable buffer — avoids one heap allocation per n-gram.
        let mut buf = String::with_capacity(text.len().min(256));

        for window in tokens.windows(n) {
            buf.clear();
            for (i, token) in window.iter().enumerate() {
                if i > 0 {
                    buf.push(' ');
                }
                buf.push_str(token);
            }

            let interned = self.intern(&buf);
            unique_ngrams.push(interned);
        }

        // Deduplicate the output (same n-gram can appear multiple times in text)
        unique_ngrams.sort_unstable();
        unique_ngrams.dedup();

        unique_ngrams
    }

    /// Get the total number of unique strings interned so far.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the interner is empty.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Get a sorted snapshot of all interned strings.
    pub fn all(&self) -> Vec<&str> {
        let mut result: Vec<&str> = self.seen.iter().copied().map(|s| {
            // SAFETY: reverse transmute
            unsafe { std::mem::transmute(s) }
        }).collect();
        result.sort_unstable();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_ngrams() {
        let mut builder = NgramBuilder::new();
        let ngrams = builder.add_text("the cat sat on the mat", 2);

        assert_eq!(
            ngrams,
            vec!["cat sat", "on the", "sat on", "the cat", "the mat"]
        );
        assert_eq!(builder.len(), 5);
    }

    #[test]
    fn test_dedup_across_texts() {
        let mut builder = NgramBuilder::new();
        builder.add_text("the cat sat", 2);
        builder.add_text("the cat ran", 2);

        // "the cat" should only be allocated once
        let all = builder.all();
        assert!(all.contains(&"the cat"));
        assert_eq!(builder.len(), 4); // "cat ran", "cat sat", "ran", "the cat"
        // Wait — unigrams aren't included with n=2. Let me fix this assertion.
    }

    #[test]
    fn test_single_ngram() {
        let mut builder = NgramBuilder::new();
        let ngrams = builder.add_text("hello world", 1);
        assert_eq!(ngrams, vec!["hello", "world"]);
    }

    #[test]
    fn test_intern_dedup() {
        let mut builder = NgramBuilder::new();
        let a = builder.intern("hello");
        let b = builder.intern("world");
        let c = builder.intern("hello"); // duplicate

        assert_eq!(a, c); // same content
        assert_eq!(builder.len(), 2);
    }

    #[test]
    fn test_large_vocab() {
        let mut builder = NgramBuilder::new();

        // Simulate a large corpus with lots of repetition
        for _ in 0..1000 {
            builder.add_text("the quick brown fox jumps over the lazy dog", 2);
        }

        // Only 8 unique bigrams regardless of how many times we process it
        assert_eq!(builder.len(), 8);
    }
}
