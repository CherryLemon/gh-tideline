//! The order of lookup results: what is closest to the file the click came from comes first.
//!
//! Hits fall into three tiers: the same file, a file in the same directory, and everything else. Among the rest, the
//! more leading directories a hit shares with the file of the click, the closer it is. The database sorts by tier,
//! shared directories, path and line, and only then cuts a long list at a fixed size, so the cut keeps the nearest hits
//! however late they sort by path.

/// How many definitions a lookup returns.
pub const MAX_DEFINITIONS: usize = 200;
/// How many references a lookup returns.
pub const MAX_REFERENCES: usize = 1000;
/// How many of the directories above the file of a click are compared with the hits. No real tree is deeper; the limit
/// keeps the SQL of a lookup from growing with the path.
const MAX_SHARED_DIRS: usize = 64;

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

    /// The directories above the file, outermost first, each with its trailing `/` and the name of the parameter that
    /// holds it for `shared_dirs_sql`: `:d1` is `src/` and `:d2` is `src/a/` for `src/a/b.rs`.
    pub fn ancestors(&self) -> Vec<(String, &'a str)> {
        self.path
            .match_indices('/')
            .take(MAX_SHARED_DIRS)
            .enumerate()
            .map(|(i, (at, _))| (format!(":d{}", i + 1), &self.path[..=at]))
            .collect()
    }
}

/// The `ORDER BY` terms that put the hits nearest to the file of the click first: by tier (the column `tier` of
/// `TIER_SQL`), by how many leading directories they share with the file, then by path and position. Takes the named
/// parameters of the `ancestors` of the `Origin`.
pub fn nearest_first_sql(ancestors: &[(String, &str)]) -> String {
    // A number alone is the position of a column in an ORDER BY: from the top of the tree there is nothing to share.
    let shared = if ancestors.is_empty() {
        String::new()
    } else {
        format!("{} DESC, ", shared_dirs_sql(ancestors))
    };
    format!("tier, {shared}te.path, t.line, t.col")
}

/// How many leading directories the path `te.path` shares with the file of the click, as SQL: in how many of its
/// `ancestors` the path lies. Each ends in a `/`, so `a/bc/x.rs` is not inside `a/b/`; they are compared as text, so
/// `_` and `%` mean themselves.
fn shared_dirs_sql(ancestors: &[(String, &str)]) -> String {
    if ancestors.is_empty() {
        return "0".to_string();
    }
    let inside: Vec<_> = ancestors
        .iter()
        .map(|(param, _)| format!("(substr(te.path, 1, length({param})) = {param})"))
        .collect();
    format!("({})", inside.join(" + "))
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, ToSql};

    use super::*;

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
    fn the_directories_above_a_file_come_outermost_first() {
        let ancestors = Origin::new("src/a/b/c.rs").ancestors();
        let expected = [(":d1", "src/"), (":d2", "src/a/"), (":d3", "src/a/b/")];
        assert_eq!(ancestors.len(), expected.len());
        for ((param, dir), (want_param, want_dir)) in ancestors.iter().zip(expected) {
            assert_eq!((param.as_str(), *dir), (want_param, want_dir));
        }
        assert!(Origin::new("c.rs").ancestors().is_empty());
        assert!(Origin::new("").ancestors().is_empty());

        // Only the outermost 64 are kept of a path that is deeper than any tree.
        let deep = "d/".repeat(100) + "x.rs";
        let ancestors = Origin::new(&deep).ancestors();
        assert_eq!(ancestors.len(), MAX_SHARED_DIRS);
        assert_eq!(ancestors[63].0, ":d64");
        assert_eq!(ancestors[63].1, &deep[..128]);
    }

    /// How many leading directories the two paths have in common, segment by segment.
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

    /// What the SQL says `path` shares with `from`.
    fn shared_by_sql(conn: &Connection, from: &str, path: &str) -> usize {
        let origin = Origin::new(from);
        let ancestors = origin.ancestors();
        let sql = format!(
            "SELECT {} FROM (SELECT :path AS path) AS te",
            shared_dirs_sql(&ancestors)
        );
        let mut params: Vec<(&str, &dyn ToSql)> = vec![(":path", &path)];
        params.extend(
            ancestors
                .iter()
                .map(|(param, dir)| (param.as_str(), dir as &dyn ToSql)),
        );
        let shared: i64 = conn
            .query_row(&sql, params.as_slice(), |row| row.get(0))
            .unwrap();
        shared as usize
    }

    #[test]
    fn directories_are_compared_by_segment_and_literally() {
        let conn = Connection::open_in_memory().unwrap();
        let cases = [
            ("a/b/c.rs", "a/b/d.rs", 2),
            ("a/b/c.rs", "a/b/x/y/d.rs", 2),
            ("a/b/c.rs", "a/x/d.rs", 1),
            ("a/b/c.rs", "x/b/c.rs", 0),
            // `a/bc` is not inside `a/b`: segments, not characters.
            ("a/b/c.rs", "a/bc/c.rs", 1),
            ("a.rs", "b.rs", 0),
            ("a.rs", "x/b.rs", 0),
            ("x/a.rs", "b.rs", 0),
            // A file name is not a directory.
            ("a/b", "a/b/c.rs", 1),
            ("a/b/c.rs", "a/b", 1),
            // `_` and `%` are no wildcards, and the case of letters counts.
            ("a_b/m.rs", "a_b/x.rs", 1),
            ("a_b/m.rs", "axb/x.rs", 0),
            ("a%b/m.rs", "a%b/x.rs", 1),
            ("a%b/m.rs", "axyb/x.rs", 0),
            ("dóm/m.rs", "dóm/x.rs", 1),
            ("dóm/m.rs", "dómx/x.rs", 0),
            ("dóm/m.rs", "dom/x.rs", 0),
            ("Src/m.rs", "src/x.rs", 0),
            ("", "a/b.rs", 0),
        ];
        for (from, path, shared) in cases {
            assert_eq!(shared_dirs(from, path), shared, "{from} {path}");
            assert_eq!(shared_by_sql(&conn, from, path), shared, "{from} {path}");
        }
    }
}
