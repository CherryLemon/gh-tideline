//! The order of lookup results: what is closest to the file the click came from comes first.
//!
//! Hits fall into three tiers: the same file, a file in the same directory, and everything else. The database sorts
//! by tier, path and line to cut a long list at a fixed size; definitions are then ranked here by how many leading
//! directories each shares with the file of the click, so the definition in the nearest part of the tree is the
//! first one.

use std::cmp::Reverse;

use super::db::Hit;

/// How many definitions the database hands over to be ranked.
pub const DEFINITION_CANDIDATES: usize = 2000;
/// How many definitions a lookup returns.
pub const MAX_DEFINITIONS: usize = 200;
/// How many references a lookup returns.
pub const MAX_REFERENCES: usize = 1000;

/// The tier of the path `te.path` as SQL: 0 for the file the click came from, 1 for a file in the same directory, 2
/// for any other. Takes the named parameters `:from` and `:dir` of an `Origin`.
pub const TIER_SQL: &str = "CASE
        WHEN te.path = :from THEN 0
        WHEN substr(te.path, 1, length(:dir)) = :dir
            AND instr(substr(te.path, length(:dir) + 1), '/') = 0 THEN 1
        ELSE 2
    END";

/// The file a click came from.
pub struct Origin<'a> {
    pub path: &'a str,
    /// The directory of `path` with its trailing `/`; empty for a file at the top of the tree.
    pub dir: &'a str,
}

impl<'a> Origin<'a> {
    pub fn new(path: &'a str) -> Origin<'a> {
        let dir = path.rfind('/').map_or("", |at| &path[..=at]);
        Origin { path, dir }
    }
}

/// The best `MAX_DEFINITIONS` of `hits`: ordered by tier, then by how many leading directories they share with the
/// file `from`, then by path and position.
pub fn rank_definitions(from: &str, mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by_cached_key(|hit| {
        (
            hit.tier,
            Reverse(shared_dirs(from, &hit.path)),
            hit.path.clone(),
            hit.line,
            hit.column,
        )
    });
    hits.truncate(MAX_DEFINITIONS);
    hits
}

/// How many leading directories the two paths have in common.
fn shared_dirs(a: &str, b: &str) -> usize {
    fn dirs(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
    dirs(a)
        .split('/')
        .zip(dirs(b).split('/'))
        .take_while(|(x, y)| x == y && !x.is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::db::Tier;

    fn hit(path: &str, line: u32, tier: Tier) -> Hit {
        Hit {
            path: path.to_string(),
            line,
            column: 1,
            end_column: 2,
            kind: "function".to_string(),
            snippet: String::new(),
            snippet_column: 0,
            tier,
        }
    }

    #[test]
    fn the_origin_knows_its_directory() {
        let cases = [
            ("a.rs", ""),
            ("src/a.rs", "src/"),
            ("src/deep/er/a.rs", "src/deep/er/"),
            ("", ""),
        ];
        for (path, dir) in cases {
            let origin = Origin::new(path);
            assert_eq!(origin.path, path);
            assert_eq!(origin.dir, dir, "{path}");
        }
    }

    #[test]
    fn directories_are_compared_by_segment() {
        assert_eq!(shared_dirs("a/b/c.rs", "a/b/d.rs"), 2);
        assert_eq!(shared_dirs("a/b/c.rs", "a/b/x/y/d.rs"), 2);
        assert_eq!(shared_dirs("a/b/c.rs", "a/x/d.rs"), 1);
        assert_eq!(shared_dirs("a/b/c.rs", "x/b/c.rs"), 0);
        // `a/bc` is not inside `a/b`: segments, not characters.
        assert_eq!(shared_dirs("a/b/c.rs", "a/bc/c.rs"), 1);
        assert_eq!(shared_dirs("a.rs", "b.rs"), 0);
        assert_eq!(shared_dirs("a.rs", "x/b.rs"), 0);
        assert_eq!(shared_dirs("x/a.rs", "b.rs"), 0);
        // A file name is not a directory.
        assert_eq!(shared_dirs("a/b", "a/b/c.rs"), 1);
    }

    #[test]
    fn definitions_are_ranked_by_tier_then_shared_directories_then_path_and_line() {
        let hits = vec![
            hit("zzz.rs", 1, Tier::Other),
            hit("src/a/z.rs", 12, Tier::Other),
            hit("src/a/b/e.rs", 7, Tier::Dir),
            hit("src/q/r.rs", 1, Tier::Other),
            hit("src/a/b/c.rs", 30, Tier::File),
            hit("lib/x.rs", 1, Tier::Other),
            hit("src/a/z.rs", 9, Tier::Other),
            hit("src/a/b/d.rs", 5, Tier::Dir),
            hit("src/a/b/c/deep.rs", 2, Tier::Other),
        ];
        let ranked = rank_definitions("src/a/b/c.rs", hits);
        let order: Vec<_> = ranked.iter().map(|h| (h.path.as_str(), h.line)).collect();
        assert_eq!(
            order,
            [
                ("src/a/b/c.rs", 30),
                ("src/a/b/d.rs", 5),
                ("src/a/b/e.rs", 7),
                // Three directories shared.
                ("src/a/b/c/deep.rs", 2),
                // Two.
                ("src/a/z.rs", 9),
                ("src/a/z.rs", 12),
                // One.
                ("src/q/r.rs", 1),
                // None, by path.
                ("lib/x.rs", 1),
                ("zzz.rs", 1),
            ]
        );
    }

    #[test]
    fn only_the_best_definitions_are_returned() {
        let hits: Vec<_> = (0..MAX_DEFINITIONS as u32 + 50)
            .map(|i| hit(&format!("f{i:04}.rs"), 1, Tier::Other))
            .collect();
        let ranked = rank_definitions("x.rs", hits);
        assert_eq!(ranked.len(), MAX_DEFINITIONS);
        assert_eq!(ranked[0].path, "f0000.rs");
        assert_eq!(ranked[MAX_DEFINITIONS - 1].path, "f0199.rs");
    }
}
