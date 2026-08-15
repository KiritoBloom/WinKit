//! In-memory relationship graph over the enumerated records.
//!
//! The whole volume is represented by IDs (file reference numbers), never by
//! paths:
//!
//! ```text
//! record: { frn, parent_frn, name, size, flags }
//! ```
//!
//! Two sorted index vectors give O(log n) parent lookup and O(children)
//! iteration:
//! * `by_frn`    — record indices sorted by file reference number.
//! * `by_parent` — record indices sorted by parent reference number, so the
//!   children of a directory form a contiguous range.
//!
//! Folder sizes are aggregated bottom-up in one pass over the in-memory
//! records — no filesystem calls (the central optimization). Because a
//! directory always exists before its children on NTFS, every child (file or
//! directory) has a higher FRN than its parent directory, so processing
//! directories in descending FRN order guarantees children are aggregated
//! before their parents.
//!
//! Full paths are only ever constructed for materialized results (top-K,
//! find), via [`PathResolver`] walking the parent chain.

use super::ntfs::FLAG_DIRECTORY;
use super::ScanRecord;

/// Bounded top-K over `(size, index)` with deterministic output order.
/// A binary min-heap keeps the `limit` largest entries; no full sort.
pub struct TopK {
    heap: std::collections::BinaryHeap<std::cmp::Reverse<Ranked>>,
    limit: usize,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Ranked {
    size: u64,
    idx: u32,
}

impl TopK {
    pub fn new(limit: usize) -> Self {
        Self {
            heap: std::collections::BinaryHeap::with_capacity(limit.max(1) + 1),
            limit: limit.max(1),
        }
    }

    #[inline]
    pub fn push(&mut self, size: u64, idx: u32) {
        if self.heap.len() < self.limit {
            self.heap.push(std::cmp::Reverse(Ranked { size, idx }));
        } else if let Some(mut smallest) = self.heap.peek_mut() {
            if size > smallest.0.size || (size == smallest.0.size && idx < smallest.0.idx) {
                *smallest = std::cmp::Reverse(Ranked { size, idx });
            }
        }
    }

    /// Sorted largest-first, ties broken by index ascending.
    pub fn into_sorted(self) -> Vec<(u64, u32)> {
        let mut v: Vec<(u64, u32)> = self
            .heap
            .into_iter()
            .map(|std::cmp::Reverse(r)| (r.size, r.idx))
            .collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        v
    }
}

/// Look up records and reconstruct paths from the flat record arrays.
/// Shared by the size pass and by query materialization; never allocates
/// per record unless a path is actually requested.
pub struct PathResolver<'a> {
    pub records: &'a [ScanRecord],
    pub names: &'a [u8],
    pub by_frn: &'a [u32],
    pub root_frn: u64,
    pub volume_root: &'a str,
}

impl<'a> PathResolver<'a> {
    /// Index of the record with file reference number `frn`, if present.
    #[inline]
    pub fn lookup(&self, frn: u64) -> Option<u32> {
        let i = self
            .by_frn
            .partition_point(|&idx| self.records[idx as usize].frn < frn);
        self.by_frn
            .get(i)
            .filter(|&&idx| self.records[idx as usize].frn == frn)
            .copied()
    }

    /// The decoded name of a record.
    #[inline]
    pub fn name_of(&self, idx: u32) -> &'a str {
        let r = &self.records[idx as usize];
        let s = r.name_off as usize;
        let e = s + r.name_len as usize;
        std::str::from_utf8(&self.names[s..e]).unwrap_or("")
    }

    /// Resolve the full physical path of a record by walking the parent
    /// chain to the root. `None` when the chain is broken or cyclic
    /// (malformed data). Only called for materialized results.
    pub fn path_of(&self, idx: u32) -> Option<String> {
        let mut parts: Vec<&str> = Vec::with_capacity(8);
        let mut cur = idx;
        let mut guard = 0usize;
        loop {
            let r = &self.records[cur as usize];
            if r.frn == self.root_frn {
                break;
            }
            parts.push(self.name_of(cur));
            if r.parent_frn == r.frn {
                break; // self-parent other than the root: malformed, stop
            }
            cur = self.lookup(r.parent_frn)?;
            guard += 1;
            if guard > 512 {
                return None; // cycle or absurd depth
            }
        }
        parts.reverse();
        let capacity = self.volume_root.len() + parts.iter().map(|p| p.len() + 1).sum::<usize>();
        let mut out = String::with_capacity(capacity);
        out.push_str(self.volume_root);
        for p in parts.iter() {
            if !out.ends_with('\\') {
                out.push('\\');
            }
            out.push_str(p);
        }
        Some(out)
    }
}

/// The relationship graph and precomputed aggregates for a snapshot.
#[derive(Debug)]
pub struct TreeIndex {
    /// Record indices sorted by `(frn, index)`.
    pub by_frn: Vec<u32>,
    /// Record indices sorted by `(parent_frn, index)`.
    pub by_parent: Vec<u32>,
    /// Per record index: for files, the file's own logical size; for
    /// directories, the sum of all descendant file sizes.
    pub aggregate: Vec<u64>,
    /// Per directory record index: descendant file count.
    pub dir_files: Vec<u64>,
    /// Per directory record index: descendant directory count (self excluded).
    pub dir_dirs: Vec<u64>,
}

impl TreeIndex {
    /// Build the index and aggregate every folder size from memory.
    pub fn build(records: &[ScanRecord]) -> Self {
        let n = records.len();
        let mut by_frn: Vec<u32> = (0..n as u32).collect();
        by_frn.sort_unstable_by(|&a, &b| {
            (records[a as usize].frn, a).cmp(&(records[b as usize].frn, b))
        });
        let mut by_parent: Vec<u32> = (0..n as u32).collect();
        by_parent.sort_unstable_by(|&a, &b| {
            (records[a as usize].parent_frn, a).cmp(&(records[b as usize].parent_frn, b))
        });

        let mut aggregate = vec![0u64; n];
        let mut dir_files = vec![0u64; n];
        let mut dir_dirs = vec![0u64; n];
        for (i, r) in records.iter().enumerate() {
            if r.flags & FLAG_DIRECTORY == 0 {
                aggregate[i] = r.size;
            }
        }
        // Directories in ascending FRN order; iterate reversed so children
        // (higher FRN) are aggregated before their parents.
        let dirs: Vec<u32> = by_frn
            .iter()
            .copied()
            .filter(|&i| records[i as usize].flags & FLAG_DIRECTORY != 0)
            .collect();
        for &d in dirs.iter().rev() {
            let parent_frn = records[d as usize].frn;
            let range = Self::parent_range(records, &by_parent, parent_frn);
            let mut sum = 0u64;
            let mut files = 0u64;
            let mut dirs_count = 0u64;
            for &c in &by_parent[range.clone()] {
                if c == d {
                    continue; // the root's parent reference points at itself
                }
                let cr = &records[c as usize];
                sum += aggregate[c as usize];
                if cr.flags & FLAG_DIRECTORY != 0 {
                    dirs_count += 1 + dir_dirs[c as usize];
                    files += dir_files[c as usize];
                } else {
                    files += 1;
                }
            }
            aggregate[d as usize] = sum;
            dir_files[d as usize] = files;
            dir_dirs[d as usize] = dirs_count;
        }

        Self {
            by_frn,
            by_parent,
            aggregate,
            dir_files,
            dir_dirs,
        }
    }

    /// The child range of the directory with file reference number `frn`.
    pub fn children_range(
        &self,
        records: &[ScanRecord],
        parent_frn: u64,
    ) -> std::ops::Range<usize> {
        Self::parent_range(records, &self.by_parent, parent_frn)
    }

    fn parent_range(
        records: &[ScanRecord],
        by_parent: &[u32],
        parent_frn: u64,
    ) -> std::ops::Range<usize> {
        let lo = by_parent.partition_point(|&c| records[c as usize].parent_frn < parent_frn);
        let hi = by_parent.partition_point(|&c| records[c as usize].parent_frn <= parent_frn);
        lo..hi
    }
}

/// Case-insensitive ASCII equality for name matching (Windows names are
/// case-insensitive).
pub fn name_eq_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Does `name` end with one of `extensions` (case-insensitive, dot excluded)?
pub fn extension_matches(name: &str, extensions: &[String]) -> bool {
    let ext = name.rsplit('.').next().unwrap_or(name);
    extensions.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// Simple wildcard match: `*` matches any sequence, `?` matches one char.
/// Without wildcards, falls back to substring containment. Case-insensitive.
pub fn pattern_matches(name: &str, pattern: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        wildcard_match(name, pattern)
    } else {
        name.to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
    }
}

fn wildcard_match(name: &str, pattern: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let pat = pattern.to_ascii_lowercase();
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while i < n.len() {
        if j < p.len() && (p[j] == '?' || p[j] == n[i]) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '*' {
            star = j;
            mark = i;
            j += 1;
        } else if star != usize::MAX {
            j = star + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '*' {
        j += 1;
    }
    j == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching() {
        assert!(pattern_matches("setup.exe", "setup.*"));
        assert!(pattern_matches("setup.exe", "*.exe"));
        assert!(pattern_matches("Archive.tar.gz", "*tar*"));
        assert!(!pattern_matches("setup.exe", "*.zip"));
        assert!(pattern_matches("file1.txt", "file?.txt"));
        assert!(!pattern_matches("file12.txt", "file?.txt"));
        assert!(pattern_matches("MyLog.TXT", "mylog.txt")); // case-insensitive substring
        assert!(pattern_matches("debug", "bug")); // substring
    }

    #[test]
    fn topk_keeps_largest_and_sorts() {
        let mut t = TopK::new(3);
        for (size, idx) in [(5, 0), (9, 1), (3, 2), (12, 3), (7, 4), (11, 5)] {
            t.push(size, idx);
        }
        let out = t.into_sorted();
        assert_eq!(out, vec![(12, 3), (11, 5), (9, 1)]);
    }

    #[test]
    fn topk_ties_broken_by_index() {
        let mut t = TopK::new(2);
        t.push(10, 5);
        t.push(10, 2);
        t.push(10, 9);
        let out = t.into_sorted();
        assert_eq!(out, vec![(10, 2), (10, 5)]);
    }
}

#[cfg(test)]
mod aggregation_tests {
    use super::*;
    use crate::platform::windows::diskscan::ntfs::FLAG_DIRECTORY;
    use crate::platform::windows::diskscan::ScanRecord;

    fn rec(frn: u64, parent: u64, size: u64, is_dir: bool) -> ScanRecord {
        ScanRecord {
            frn,
            parent_frn: parent,
            size,
            mtime: 0,
            name_off: 0,
            name_len: 0,
            attributes: 0,
            flags: if is_dir { FLAG_DIRECTORY } else { 0 },
        }
    }

    #[test]
    fn aggregates_bottom_up_without_rescanning() {
        // root(5) ─ A(10) ─ f40=200, f50=300
        //         └ B(20) ─ f30=100
        let records = vec![
            rec(5, 5, 0, true),
            rec(10, 5, 0, true),
            rec(20, 5, 0, true),
            rec(30, 20, 100, false),
            rec(40, 10, 200, false),
            rec(50, 10, 300, false),
        ];
        let idx = TreeIndex::build(&records);
        // Index order follows the input order; find by frn.
        let idx_of = |frn: u64| records.iter().position(|r| r.frn == frn).unwrap();
        assert_eq!(idx.aggregate[idx_of(10)], 500);
        assert_eq!(idx.aggregate[idx_of(20)], 100);
        assert_eq!(idx.aggregate[idx_of(5)], 600);
        assert_eq!(idx.dir_files[idx_of(10)], 2);
        assert_eq!(idx.dir_files[idx_of(5)], 3);
        assert_eq!(idx.dir_dirs[idx_of(5)], 2);
        // Children ranges are contiguous.
        let range = idx.children_range(&records, 10);
        let mut kids: Vec<u64> = idx.by_parent[range]
            .iter()
            .map(|&c| records[c as usize].frn)
            .collect();
        kids.sort_unstable();
        assert_eq!(kids, vec![40, 50]);
    }

    #[test]
    fn deep_nesting_aggregates_correctly() {
        // Chain of 10 nested dirs with one 42-byte file at the bottom.
        let mut records = vec![rec(5, 5, 0, true)];
        let mut prev = 5u64;
        for i in 1..=10 {
            let id = 10 + i;
            records.push(rec(id, prev, 0, true));
            prev = id;
        }
        records.push(rec(1000, prev, 42, false));
        let idx = TreeIndex::build(&records);
        assert_eq!(idx.aggregate[0], 42);
        // Every dir in the chain reports 42 and one file.
        for i in 1..=10 {
            assert_eq!(idx.aggregate[i], 42);
            assert_eq!(idx.dir_files[i], 1);
            assert_eq!(idx.dir_dirs[i], (10 - i) as u64);
        }
    }

    #[test]
    fn orphan_attached_to_root_still_aggregates() {
        // File under a missing parent: parent rewritten to root.
        let mut records = vec![rec(5, 5, 0, true), rec(60, 999, 7, false)];
        records[1].parent_frn = 5;
        let idx = TreeIndex::build(&records);
        assert_eq!(idx.aggregate[0], 7);
        assert_eq!(idx.dir_files[0], 1);
    }
}
