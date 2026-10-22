//! Parsers for git plumbing output: NUL-separated records, `cat-file --batch` records and
//! newline-separated lists.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::is_oid;

pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";
pub const GITLINK_MODE: &str = "160000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: String,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub old_mode: String,
    pub new_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    pub binary: bool,
    pub generated: bool,
}

/// Parses `git diff-tree -r -z --raw [--numstat] --no-abbrev [-M]` output.
///
/// Raw records come first (`:<old_mode> <new_mode> <old_oid> <new_oid> <status>\0<path>\0`,
/// with a second path for R/C), followed by numstat records
/// (`<add>\t<del>\t<path>\0`, or `<add>\t<del>\t\0<src>\0<dst>\0` for renames/copies).
/// Numstat values are attached to the raw entry with the same destination path.
pub fn parse_diff_tree(out: &[u8]) -> Result<Vec<ChangedFile>, String> {
    let mut tokens = out.split(|&b| b == 0).peekable();
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut numstat: HashMap<String, (Option<u64>, Option<u64>)> = HashMap::new();

    while let Some(tok) = tokens.next() {
        if tok.is_empty() && tokens.peek().is_none() {
            break;
        }
        let mut next_path = |what: &str| -> Result<String, String> {
            tokens
                .next()
                .filter(|t| !t.is_empty())
                .map(|t| String::from_utf8_lossy(t).into_owned())
                .ok_or_else(|| format!("diff-tree output truncated: missing {what}"))
        };
        if let Some(header) = tok.strip_prefix(b":") {
            let header = std::str::from_utf8(header)
                .map_err(|_| "diff-tree raw header is not UTF-8".to_string())?;
            let fields: Vec<&str> = header.split(' ').collect();
            if fields.len() != 5 {
                return Err(format!("bad diff-tree raw header: {header:?}"));
            }
            let status = fields[4]
                .chars()
                .next()
                .ok_or_else(|| format!("empty status in {header:?}"))?;
            let (old_path, path) = if status == 'R' || status == 'C' {
                let src = next_path("source path")?;
                let dst = next_path("destination path")?;
                (Some(src), dst)
            } else {
                (None, next_path("path")?)
            };
            files.push(ChangedFile {
                path,
                old_path,
                status: status.to_string(),
                old_oid: non_zero_oid(fields[2]),
                new_oid: non_zero_oid(fields[3]),
                old_mode: fields[0].to_string(),
                new_mode: fields[1].to_string(),
                additions: None,
                deletions: None,
                binary: false,
                generated: false,
            });
        } else {
            // Split on bytes: the path need not be UTF-8 and is decoded lossily, exactly
            // like raw-record paths, so the two still match up.
            let bad = || format!("bad numstat record: {:?}", String::from_utf8_lossy(tok));
            let mut parts = tok.splitn(3, |&b| b == b'\t');
            let add = parts.next().unwrap_or_default();
            let del = parts.next().ok_or_else(bad)?;
            let inline_path = parts.next().ok_or_else(bad)?;
            let path = if inline_path.is_empty() {
                next_path("numstat source path")?;
                next_path("numstat destination path")?
            } else {
                String::from_utf8_lossy(inline_path).into_owned()
            };
            numstat.insert(path, (parse_count(add)?, parse_count(del)?));
        }
    }

    for f in &mut files {
        if let Some(&(add, del)) = numstat.get(&f.path) {
            f.binary = add.is_none() && del.is_none();
            f.additions = add;
            f.deletions = del;
        }
    }
    Ok(files)
}

fn non_zero_oid(oid: &str) -> Option<String> {
    (oid != ZERO_OID).then(|| oid.to_string())
}

fn parse_count(s: &[u8]) -> Result<Option<u64>, String> {
    if s == b"-" {
        return Ok(None);
    }
    std::str::from_utf8(s)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| format!("bad numstat count: {:?}", String::from_utf8_lossy(s)))
}

/// One file of a commit's tree: a blob, or a gitlink (mode 160000) for a submodule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    pub oid: String,
}

/// Parses `git ls-tree -r -z` output: `<mode> SP <type> SP <oid> TAB <path>\0` records.
/// Blobs and gitlinks are kept (`-r` lists no trees).
pub fn parse_ls_tree(out: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut entries = Vec::new();
    for record in out.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let bad = || format!("bad ls-tree record: {:?}", String::from_utf8_lossy(record));
        let tab = record.iter().position(|&b| b == b'\t').ok_or_else(bad)?;
        let header = std::str::from_utf8(&record[..tab]).map_err(|_| bad())?;
        let mut fields = header.split(' ');
        let (Some(mode), Some(kind), Some(oid), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(bad());
        };
        if kind != "blob" && kind != "commit" {
            continue;
        }
        entries.push(TreeEntry {
            path: String::from_utf8_lossy(&record[tab + 1..]).into_owned(),
            mode: mode.to_string(),
            oid: oid.to_string(),
        });
    }
    Ok(entries)
}

/// Parses `git check-attr -z` output (`<path>\0<attr>\0<value>\0` triplets) into
/// `path -> [(attr, value)]`.
pub fn parse_check_attr(out: &[u8]) -> HashMap<String, Vec<(String, String)>> {
    let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut tokens = out.split(|&b| b == 0);
    while let (Some(path), Some(attr), Some(value)) = (tokens.next(), tokens.next(), tokens.next())
    {
        map.entry(String::from_utf8_lossy(path).into_owned())
            .or_default()
            .push((
                String::from_utf8_lossy(attr).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            ));
    }
    map
}

/// Splits the next `git cat-file --batch` record off `out`: `Ok(None)` for a
/// `<name> missing` / `<name> ambiguous` line, else `(oid, type, contents)`.
#[allow(clippy::type_complexity)]
pub(crate) fn next_batch_record<'a>(
    out: &mut &'a [u8],
) -> Result<Option<(&'a str, &'a str, &'a [u8])>, String> {
    let nl = out
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("cat-file output truncated: missing header")?;
    let header = &out[..nl];
    *out = &out[nl + 1..];
    if header.ends_with(b" missing") || header.ends_with(b" ambiguous") {
        return Ok(None);
    }
    let header = std::str::from_utf8(header)
        .map_err(|_| format!("bad cat-file header: {:?}", String::from_utf8_lossy(header)))?;
    let mut fields = header.split(' ');
    let (Some(oid), Some(kind), Some(size), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(format!("bad cat-file header: {header:?}"));
    };
    let size: usize = size
        .parse()
        .map_err(|_| format!("bad cat-file header: {header:?}"))?;
    if out.len() <= size || out[size] != b'\n' {
        return Err("cat-file output truncated: short contents".into());
    }
    let data = &out[..size];
    *out = &out[size + 1..];
    Ok(Some((oid, kind, data)))
}

/// Parses `git cat-file --batch` output for a single requested object.
pub fn parse_batch_object(out: &[u8]) -> Result<super::ObjectLookup, String> {
    let mut rest = out;
    Ok(match next_batch_record(&mut rest)? {
        None => super::ObjectLookup::Missing,
        Some((_, kind, data)) => super::ObjectLookup::Found {
            kind: kind.to_string(),
            data: data.to_vec(),
        },
    })
}

/// Parses `git cat-file --batch` output for a list of objects into `(oid, contents)` for each
/// blob, in output order. Missing objects and objects of other types are skipped.
pub fn parse_batch_blobs(out: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut rest = out;
    let mut blobs = Vec::new();
    while !rest.is_empty() {
        if let Some((oid, "blob", data)) = next_batch_record(&mut rest)? {
            blobs.push((oid.to_string(), data.to_vec()));
        }
    }
    Ok(blobs)
}

/// Parses `git cat-file --batch-check='%(objectname) %(objectsize)'` output into `oid -> size`.
/// `<name> missing` and `<name> ambiguous` lines are skipped, as in [`next_batch_record`].
pub fn parse_object_sizes(out: &[u8]) -> Result<HashMap<String, u64>, String> {
    let mut sizes = HashMap::new();
    for line in out.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if line.ends_with(b" missing") || line.ends_with(b" ambiguous") {
            continue;
        }
        let bad = || format!("bad cat-file line: {:?}", String::from_utf8_lossy(line));
        let line = std::str::from_utf8(line).map_err(|_| bad())?;
        let (oid, size) = line.split_once(' ').ok_or_else(bad)?;
        sizes.insert(oid.to_string(), size.parse().map_err(|_| bad())?);
    }
    Ok(sizes)
}

/// Parses `git rev-list --objects --no-walk --missing=print --no-object-names` output into the
/// OIDs of the objects that are missing locally. A present object is printed as `<oid>`, a
/// missing one as `?<oid>`; any other line is an error.
pub fn parse_missing_objects(out: &[u8]) -> Result<HashSet<String>, String> {
    let out = std::str::from_utf8(out).map_err(|_| "rev-list output is not UTF-8")?;
    let mut missing = HashSet::new();
    for line in out.split_terminator('\n') {
        let (oid, is_missing) = match line.strip_prefix('?') {
            Some(oid) => (oid, true),
            None => (line, false),
        };
        if !is_oid(oid) {
            return Err(format!("bad rev-list line: {line:?}"));
        }
        if is_missing {
            missing.insert(oid.to_string());
        }
    }
    Ok(missing)
}

/// Whether the stderr of a failed `git cat-file` says that it died on an object that a tree
/// promises but the clone never fetched: `fatal: could not fetch <oid> from promisor remote`.
pub fn is_unfetched_object_error(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        line.strip_prefix("fatal: could not fetch ")
            .and_then(|rest| rest.strip_suffix(" from promisor remote"))
            .is_some_and(is_oid)
    })
}

/// The objects that the stderr of a failed `git fetch` says the remote refuses to give, in the order it names them and
/// without repeats. Two messages name them. `fatal: remote error: upload-pack: not our ref <oid>` comes from a server
/// that allows a request for an object that is no tip of its refs, which it does not have (protocol v2, or v0 against a
/// server that allows any SHA-1 in a want, as GitHub's does): it stops at the first object it does not have, so the
/// objects after that one in the request are neither refused nor known to be fine. `error: Server does not allow
/// request for unadvertised object <oid>` comes from the client, against a v0 server that allows no request for an
/// object that is no tip: a line for every object of the request, those the server has included, so that all are named,
/// and none of them can be had from such a server.
pub fn refused_oids(stderr: &str) -> Vec<String> {
    const MARKERS: [&str; 2] = [
        "not our ref ",
        "Server does not allow request for unadvertised object ",
    ];
    let mut refused: Vec<String> = Vec::new();
    for line in stderr.lines() {
        for marker in MARKERS {
            let oid = line.rsplit_once(marker).map(|(_, oid)| oid.trim_end());
            if let Some(oid) = oid.filter(|oid| is_oid(oid))
                && !refused.iter().any(|seen| seen == oid)
            {
                refused.push(oid.to_string());
            }
        }
    }
    refused
}

/// The entries of `oids` that `listing` names, in the order of `oids` and with their repeats.
/// `listing` is `git cat-file --batch-check=%(objectname) --batch-all-objects` output: one OID
/// per line, in any order, repeats allowed.
pub fn listed_oids(oids: &[String], listing: &[u8]) -> Result<Vec<String>, String> {
    let listing = std::str::from_utf8(listing).map_err(|_| "cat-file listing is not UTF-8")?;
    let mut local: HashMap<&str, bool> = oids.iter().map(|oid| (oid.as_str(), false)).collect();
    for line in listing.split_terminator('\n') {
        if !is_oid(line) {
            return Err(format!("bad cat-file line: {line:?}"));
        }
        if let Some(found) = local.get_mut(line) {
            *found = true;
        }
    }
    Ok(oids
        .iter()
        .filter(|oid| local[oid.as_str()])
        .cloned()
        .collect())
}

/// Parses `git cat-file --batch` output for a list of trees and returns the OIDs of their
/// regular-file (mode 100644/100755) `.gitattributes` entries. Missing objects and
/// non-trees are skipped.
pub fn gitattributes_in_trees(out: &[u8]) -> Result<Vec<String>, String> {
    let mut rest = out;
    let mut oids = Vec::new();
    while !rest.is_empty() {
        let Some((tree_oid, kind, mut data)) = next_batch_record(&mut rest)? else {
            continue;
        };
        if kind != "tree" {
            continue;
        }
        // Raw tree entries: `<mode> SP <name> NUL <binary oid>`.
        let oid_len = tree_oid.len() / 2;
        while !data.is_empty() {
            let bad = || format!("malformed tree {tree_oid}");
            let sp = data.iter().position(|&b| b == b' ').ok_or_else(bad)?;
            let nul = data.iter().position(|&b| b == 0).ok_or_else(bad)?;
            if nul < sp || data.len() < nul + 1 + oid_len {
                return Err(bad());
            }
            let (mode, name) = (&data[..sp], &data[sp + 1..nul]);
            if name == b".gitattributes" && (mode == b"100644" || mode == b"100755") {
                oids.push(hex::encode(&data[nul + 1..nul + 1 + oid_len]));
            }
            data = &data[nul + 1 + oid_len..];
        }
    }
    Ok(oids)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const C: &str = "3333333333333333333333333333333333333333";

    fn raw(old_mode: &str, new_mode: &str, old: &str, new: &str, status: &str) -> String {
        format!(":{old_mode} {new_mode} {old} {new} {status}\0")
    }

    #[test]
    fn modified_file_with_numstat() {
        let out = format!(
            "{}src/main.rs\0{}",
            raw("100644", "100644", A, B, "M"),
            "12\t3\tsrc/main.rs\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(
            files,
            vec![ChangedFile {
                path: "src/main.rs".into(),
                old_path: None,
                status: "M".into(),
                old_oid: Some(A.into()),
                new_oid: Some(B.into()),
                old_mode: "100644".into(),
                new_mode: "100644".into(),
                additions: Some(12),
                deletions: Some(3),
                binary: false,
                generated: false,
            }]
        );
    }

    #[test]
    fn rename_with_score_and_numstat_pair() {
        let out = format!(
            "{}old/name.txt\0new/name.txt\0{}",
            raw("100644", "100644", A, B, "R085"),
            "1\t0\t\0old/name.txt\0new/name.txt\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, "R");
        assert_eq!(f.path, "new/name.txt");
        assert_eq!(f.old_path.as_deref(), Some("old/name.txt"));
        assert_eq!((f.additions, f.deletions), (Some(1), Some(0)));
        assert!(!f.binary);
    }

    #[test]
    fn copy_status_keeps_old_path() {
        let out = format!(
            "{}a.txt\0b.txt\0{}",
            raw("100644", "100644", A, A, "C100"),
            "0\t0\t\0a.txt\0b.txt\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(files[0].status, "C");
        assert_eq!(files[0].old_path.as_deref(), Some("a.txt"));
        assert_eq!(files[0].path, "b.txt");
    }

    #[test]
    fn binary_numstat_marks_binary_without_counts() {
        let out = format!(
            "{}img/logo.png\0{}",
            raw("100644", "100644", A, B, "M"),
            "-\t-\timg/logo.png\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert!(files[0].binary);
        assert_eq!((files[0].additions, files[0].deletions), (None, None));
    }

    #[test]
    fn added_and_deleted_files_have_null_oids() {
        let out = format!(
            "{}new.txt\0{}gone.txt\0{}{}",
            raw("000000", "100644", ZERO_OID, B, "A"),
            raw("100644", "000000", A, ZERO_OID, "D"),
            "3\t0\tnew.txt\0",
            "0\t5\tgone.txt\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(files[0].status, "A");
        assert_eq!(files[0].old_oid, None);
        assert_eq!(files[0].new_oid.as_deref(), Some(B));
        assert_eq!(files[0].old_mode, "000000");
        assert_eq!(files[1].status, "D");
        assert_eq!(files[1].old_oid.as_deref(), Some(A));
        assert_eq!(files[1].new_oid, None);
        assert_eq!((files[1].additions, files[1].deletions), (Some(0), Some(5)));
    }

    #[test]
    fn type_change_and_mode_only_change() {
        // Regular file -> symlink is reported as T; chmod +x alone is M with identical oids.
        let out = format!(
            "{}link\0{}run.sh\0{}{}",
            raw("100644", "120000", A, C, "T"),
            raw("100644", "100755", B, B, "M"),
            "1\t1\tlink\0",
            "0\t0\trun.sh\0"
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(files[0].status, "T");
        assert_eq!(
            (files[0].old_mode.as_str(), files[0].new_mode.as_str()),
            ("100644", "120000")
        );
        assert_eq!(files[1].status, "M");
        assert_eq!(files[1].old_oid, files[1].new_oid);
        assert_eq!(
            (files[1].old_mode.as_str(), files[1].new_mode.as_str()),
            ("100644", "100755")
        );
        assert_eq!((files[1].additions, files[1].deletions), (Some(0), Some(0)));
        assert!(!files[1].binary);
    }

    #[test]
    fn raw_only_output_and_odd_paths() {
        // --no-renames output without numstat; -z means paths are not quoted.
        let out = format!(
            "{}dir/with space\ttab.txt\0{}:colon.txt\0",
            raw("100644", "100644", A, B, "M"),
            raw("000000", "100644", ZERO_OID, C, "A"),
        );
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "dir/with space\ttab.txt");
        assert_eq!(files[1].path, ":colon.txt");
        assert_eq!(files[0].additions, None);
        assert!(!files[0].binary);
    }

    #[test]
    fn empty_output_is_empty_list() {
        assert!(parse_diff_tree(b"").unwrap().is_empty());
    }

    #[test]
    fn truncated_output_is_an_error() {
        let out = raw("100644", "100644", A, B, "R100");
        assert!(parse_diff_tree(format!("{out}only-src\0").as_bytes()).is_err());
        assert!(parse_diff_tree(b":100644 100644 abc M\0x\0").is_err());
    }

    #[test]
    fn real_git_output() {
        // Captured verbatim from `git diff-tree -r -z -M --raw --numstat --no-abbrev` (git 2.43)
        // on a scratch repo: binary edit, delete, chmod, rename+edit, add with a space.
        let out = ":100644 100644 88768efdf77ec78c9a995f94881793be6a41752b 3e3315e1b02129d197721a8a0b56dd88862f454d M\0b.bin\0:100644 000000 587be6b4c3f93f93c489c0111bba5596147a26cb 0000000000000000000000000000000000000000 D\0del.txt\0:100644 100755 45b983be36b73c0788dc9cbcb76cbb80fc7bb057 45b983be36b73c0788dc9cbcb76cbb80fc7bb057 M\0mode.sh\0:100644 100644 0fdf397db08b5cecda1b6394d4fef7395c1933ba f9d9a0195c5b9c01ef64e2a69d8b9a624f42b8c8 R085\0keep.txt\0moved.txt\0:000000 100644 0000000000000000000000000000000000000000 3e757656cf36eca53338e520d134963a44f793f8 A\0sp ace.txt\0-\t-\tb.bin\x000\t1\tdel.txt\x000\t0\tmode.sh\x001\t0\t\0keep.txt\0moved.txt\x001\t0\tsp ace.txt\0";
        let files = parse_diff_tree(out.as_bytes()).unwrap();
        let summary: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    f.status.as_str(),
                    f.path.as_str(),
                    f.old_path.as_deref(),
                    f.additions,
                    f.deletions,
                    f.binary,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("M", "b.bin", None, None, None, true),
                ("D", "del.txt", None, Some(0), Some(1), false),
                ("M", "mode.sh", None, Some(0), Some(0), false),
                ("R", "moved.txt", Some("keep.txt"), Some(1), Some(0), false),
                ("A", "sp ace.txt", None, Some(1), Some(0), false),
            ]
        );
        assert_eq!(files[1].new_oid, None);
        assert_eq!(files[4].old_oid, None);
        assert_eq!(files[2].new_mode, "100755");
    }

    #[test]
    fn check_attr_triplets() {
        let out = b"a.lock\0linguist-generated\0unspecified\0a.lock\0linguist-vendored\0set\0gen.go\0linguist-generated\0true\0";
        let map = parse_check_attr(out);
        assert_eq!(map["a.lock"].len(), 2);
        assert_eq!(
            map["a.lock"][1],
            ("linguist-vendored".to_string(), "set".to_string())
        );
        assert_eq!(map["gen.go"][0].1, "true");
    }

    #[test]
    fn non_utf8_numstat_path() {
        // A Latin-1 file name: the PR must still open, with counts attached to the file.
        let mut out = raw("100644", "100644", A, B, "M").into_bytes();
        out.extend_from_slice(b"caf\xe9.txt\0");
        out.extend_from_slice(raw("100644", "100644", A, C, "M").as_bytes());
        out.extend_from_slice(b"ok.txt\0");
        out.extend_from_slice(b"4\t2\tcaf\xe9.txt\0");
        out.extend_from_slice(b"1\t0\tok.txt\0");
        let files = parse_diff_tree(&out).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "caf\u{fffd}.txt");
        assert_eq!((files[0].additions, files[0].deletions), (Some(4), Some(2)));
        assert_eq!((files[1].additions, files[1].deletions), (Some(1), Some(0)));
        // Same for a rename pair.
        let mut out = raw("100644", "100644", A, B, "R090").into_bytes();
        out.extend_from_slice(b"\xff/old\0\xff/new\0");
        out.extend_from_slice(b"3\t1\t\0\xff/old\0\xff/new\0");
        let files = parse_diff_tree(&out).unwrap();
        assert_eq!(files[0].path, "\u{fffd}/new");
        assert_eq!(files[0].additions, Some(3));
    }

    #[test]
    fn ls_tree_records() {
        let out = format!(
            "100644 blob {A}\tsrc/a b.rs\0160000 commit {B}\tvendor/lib\0120000 blob {C}\tlink\0"
        );
        let mut out = out.into_bytes();
        out.extend_from_slice(format!("100644 blob {A}\ttab\there\0").as_bytes());
        out.extend_from_slice(format!("040000 tree {B}\tdir\0").as_bytes());
        let entries = parse_ls_tree(&out).unwrap();
        let got: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.mode.as_str(), e.oid.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("src/a b.rs", "100644", A),
                ("vendor/lib", "160000", B),
                ("link", "120000", C),
                ("tab\there", "100644", A),
            ]
        );
        assert!(parse_ls_tree(b"").unwrap().is_empty());
        assert!(parse_ls_tree(b"100644 blob\tno-oid\0").is_err());
        assert!(parse_ls_tree(b"no tab\0").is_err());
    }

    #[test]
    fn batch_object_records() {
        match parse_batch_object(format!("{A} blob 3\nab\n\n").as_bytes()).unwrap() {
            crate::git::ObjectLookup::Found { kind, data } => {
                assert_eq!(kind, "blob");
                assert_eq!(data, b"ab\n");
            }
            crate::git::ObjectLookup::Missing => panic!("expected a blob"),
        }
        assert!(matches!(
            parse_batch_object(format!("{A} missing\n").as_bytes()).unwrap(),
            crate::git::ObjectLookup::Missing
        ));
        assert!(parse_batch_object(format!("{A} blob 10\nab\n").as_bytes()).is_err());
        assert!(parse_batch_object(b"").is_err());
    }

    #[test]
    fn gitattributes_from_tree_records() {
        let oid = |b: u8| [b; 20];
        let mut tree = Vec::new();
        for (mode, name, o) in [
            ("100644", ".gitattributes", oid(0x11)),
            ("40000", "src", oid(0x22)),
            ("120000", "link", oid(0x33)),
        ] {
            tree.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            tree.extend_from_slice(&o);
        }
        let mut sub = Vec::new();
        sub.extend_from_slice(b"40000 .gitattributes\0");
        sub.extend_from_slice(&oid(0x44));
        sub.extend_from_slice(b"100755 .gitattributes\0");
        sub.extend_from_slice(&oid(0x55));
        let mut out = Vec::new();
        out.extend_from_slice(format!("{A} tree {}\n", tree.len()).as_bytes());
        out.extend_from_slice(&tree);
        out.extend_from_slice(format!("\n{B}:gone dir missing\n").as_bytes());
        out.extend_from_slice(format!("{C} tree {}\n", sub.len()).as_bytes());
        out.extend_from_slice(&sub);
        out.push(b'\n');
        assert_eq!(
            gitattributes_in_trees(&out).unwrap(),
            vec!["11".repeat(20), "55".repeat(20)]
        );
        assert!(gitattributes_in_trees(format!("{A} tree 5\n100644 x\n").as_bytes()).is_err());
    }

    #[test]
    fn missing_objects_are_the_question_mark_lines() {
        // Captured verbatim from `git rev-list --objects --no-walk --missing=print
        // --no-object-names` (git 2.43) on a blob-less clone with two of four blobs fetched: the
        // commit, its trees and the fetched blobs come bare, the other blobs as `?<oid>`.
        let out = "c51fe0c261dd96e681500d433d7dcb10f2896150\n700fed341f332807aefa8ad97cddf5cc9ecfd652\n5626abf0f72e58d7a153368ba57db4c673c0e171\n89ccbb93e4ade2cbb83d93c4af3b33bff1175add\nc84124ea9e02701fc1038a635a8604b62ebf2c75\n30c4a80d8dd59a2fcbf86cd5fd0973dbb8bc6d6c\n?8510665149157c2bc901848c3e0b746954e9cbd9\n?f719efd430d52bcfc8566a43b2eb655688d38871\n";
        let want: HashSet<String> = [
            "8510665149157c2bc901848c3e0b746954e9cbd9",
            "f719efd430d52bcfc8566a43b2eb655688d38871",
        ]
        .map(String::from)
        .into();
        assert_eq!(parse_missing_objects(out.as_bytes()).unwrap(), want);

        // A repeated OID is one entry, the last newline is optional, nothing is no output.
        assert_eq!(
            parse_missing_objects(format!("{A}\n?{B}\n?{B}").as_bytes()).unwrap(),
            HashSet::from([B.to_string()])
        );
        assert!(parse_missing_objects(b"").unwrap().is_empty());
        assert!(
            parse_missing_objects(format!("{A}\n{B}\n").as_bytes())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_missing_object_lines_are_errors() {
        let upper = format!("?{}\n", "A".repeat(40));
        for bad in [
            "\n",
            "?\n",
            "??1111111111111111111111111111111111111111\n",
            "?abc\n",
            "? 1111111111111111111111111111111111111111\n",
            "?11111111111111111111111111111111111111111\n",
            "1111111111111111111111111111111111111111\n\n2222222222222222222222222222222222222222\n",
            // Without `--no-object-names` every present object comes with a name.
            "1111111111111111111111111111111111111111 path\n",
            "?1111111111111111111111111111111111111111 path\n",
            upper.as_str(),
        ] {
            assert!(parse_missing_objects(bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert!(parse_missing_objects(b"?\xff\xff\n").is_err());
    }

    #[test]
    fn object_size_lines() {
        // Captured from `git cat-file --batch-check='%(objectname) %(objectsize)'` (git 2.43).
        let out = "5626abf0f72e58d7a153368ba57db4c673c0e171 4\n1234567890123456789012345678901234567891 missing\n2bdf67abb163a4ffb2d7f3f0880c9fe5068ce782 6\n";
        let sizes = parse_object_sizes(out.as_bytes()).unwrap();
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes["5626abf0f72e58d7a153368ba57db4c673c0e171"], 4);
        assert_eq!(sizes["2bdf67abb163a4ffb2d7f3f0880c9fe5068ce782"], 6);

        let sizes = parse_object_sizes(format!("{A} 0\n{B} 5000000000").as_bytes()).unwrap();
        assert_eq!((sizes[A], sizes[B]), (0, 5_000_000_000));
        // `ambiguous` is skipped like `missing`, as `next_batch_record` does.
        let sizes =
            parse_object_sizes(format!("{A} 1\n{B} ambiguous\n{C} missing\n").as_bytes()).unwrap();
        assert_eq!(sizes, HashMap::from([(A.to_string(), 1)]));
        assert!(parse_object_sizes(b"").unwrap().is_empty());
        assert!(
            parse_object_sizes(format!("{A} missing\n").as_bytes())
                .unwrap()
                .is_empty()
        );
        for bad in [
            format!("{A}\n"),
            format!("{A} big\n"),
            format!("{A} -1\n"),
            format!("{A} 1 2\n"),
            format!("{A} 4\n{B} \n"),
        ] {
            assert!(parse_object_sizes(bad.as_bytes()).is_err(), "{bad:?}");
        }
    }

    /// Appends one `git cat-file --batch` record.
    fn batch_record(out: &mut Vec<u8>, oid: &str, kind: &str, data: &[u8]) {
        out.extend_from_slice(format!("{oid} {kind} {}\n", data.len()).as_bytes());
        out.extend_from_slice(data);
        out.push(b'\n');
    }

    #[test]
    fn batch_blobs_skip_missing_objects_and_other_types() {
        // Captured from `git cat-file --batch` (git 2.43): a blob, an object git has never
        // heard of, a blob.
        let out = "5626abf0f72e58d7a153368ba57db4c673c0e171 blob 4\none\n\n1234567890123456789012345678901234567891 missing\n2bdf67abb163a4ffb2d7f3f0880c9fe5068ce782 blob 6\nthree\n\n";
        assert_eq!(
            parse_batch_blobs(out.as_bytes()).unwrap(),
            vec![
                (
                    "5626abf0f72e58d7a153368ba57db4c673c0e171".to_string(),
                    b"one\n".to_vec()
                ),
                (
                    "2bdf67abb163a4ffb2d7f3f0880c9fe5068ce782".to_string(),
                    b"three\n".to_vec()
                ),
            ]
        );

        // Contents that look like records, are empty or binary come back byte for byte.
        let (d, e, f, g) = (
            "4".repeat(40),
            "5".repeat(40),
            "6".repeat(40),
            "7".repeat(40),
        );
        let lookalike = format!("{B} blob 3\nxyz\n{C} missing\n");
        let binary: &[u8] = b"\0\xff\xfe\r\n\n\0";
        let mut out = Vec::new();
        batch_record(&mut out, A, "blob", b"ab\n");
        out.extend_from_slice(format!("{B} missing\n{C} ambiguous\n").as_bytes());
        batch_record(&mut out, &d, "tree", b"100644 x\0--------------------");
        batch_record(&mut out, &e, "commit", b"tree 1\n\nmessage\n");
        batch_record(&mut out, &f, "blob", lookalike.as_bytes());
        batch_record(&mut out, &g, "blob", b"");
        batch_record(&mut out, A, "blob", binary);
        let got = parse_batch_blobs(&out).unwrap();
        let expected = [
            (A, b"ab\n".as_slice()),
            (f.as_str(), lookalike.as_bytes()),
            (g.as_str(), b"".as_slice()),
            (A, binary),
        ];
        assert_eq!(got.len(), expected.len());
        for ((oid, data), (want_oid, want_data)) in got.iter().zip(expected) {
            assert_eq!((oid.as_str(), data.as_slice()), (want_oid, want_data));
        }
        assert!(parse_batch_blobs(b"").unwrap().is_empty());
        assert!(
            parse_batch_blobs(format!("{A} missing\n").as_bytes())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn truncated_batch_blobs_are_an_error() {
        let mut one = Vec::new();
        batch_record(&mut one, A, "blob", b"abcdef");
        let mut two = one.clone();
        batch_record(&mut two, B, "blob", b"ghijkl");
        // Cut anywhere inside the second record: the header, the contents, the newline after them.
        for cut in one.len() + 1..two.len() {
            assert!(parse_batch_blobs(&two[..cut]).is_err(), "cut at {cut}");
        }
        // Cut between the records, the output is just shorter.
        assert_eq!(parse_batch_blobs(&two[..one.len()]).unwrap().len(), 1);
        assert!(parse_batch_blobs(format!("{A} blob 3").as_bytes()).is_err());
        assert!(parse_batch_blobs(format!("{A} blob x\nabc\n").as_bytes()).is_err());
        assert!(parse_batch_blobs(b"garbage\n").is_err());
        // A size that no output can have is an error, not an overflow of `size + 1`.
        for size in [usize::MAX, usize::MAX - 1] {
            assert!(parse_batch_blobs(format!("{A} blob {size}\nabc\n").as_bytes()).is_err());
        }
    }

    #[test]
    fn promisor_fetch_failure_is_recognized() {
        // Captured from `git cat-file --batch` under `-c protocol.allow=never` (git 2.43);
        // `run_git` trims the final newline.
        let stderr = "fatal: transport 'file' not allowed\nfatal: could not fetch f719efd430d52bcfc8566a43b2eb655688d38871 from promisor remote";
        assert!(is_unfetched_object_error(stderr));
        assert!(is_unfetched_object_error(&format!(
            "fatal: could not fetch {A} from promisor remote\n"
        )));
        for other in [
            "",
            "fatal: transport 'file' not allowed",
            "fatal: not a git repository: 'nowhere.git'",
            "fatal: could not fetch HEAD from promisor remote",
            "fatal: could not fetch  from promisor remote",
            "fatal: could not fetch",
        ] {
            assert!(!is_unfetched_object_error(other), "{other:?}");
        }
    }

    #[test]
    fn the_objects_a_remote_refuses_are_named_in_the_stderr_of_a_fetch() {
        // Captured from `git fetch --stdin` (git 2.43) for an object that a local remote lacks, under protocol v2: the
        // server names the first object it refuses, once for itself and once as the client relays it; `run_git` trims
        // the final newline.
        let v2 = "fatal: git upload-pack: not our ref 1234567890123456789012345678901234567891\nfatal: remote error: upload-pack: not our ref 1234567890123456789012345678901234567891";
        assert_eq!(
            refused_oids(v2),
            ["1234567890123456789012345678901234567891"]
        );
        // Captured under protocol v0 against a remote that allows no object that is not a tip: the client names them all.
        let v0 = format!(
            "error: Server does not allow request for unadvertised object {B}\nerror: Server does not allow request for unadvertised object {A}\nerror: Server does not allow request for unadvertised object {B}\n"
        );
        assert_eq!(refused_oids(&v0), [B, A]);
        // Whatever precedes the message does not matter.
        assert_eq!(
            refused_oids(&format!("remote: error: upload-pack: not our ref {C} \n")),
            [C]
        );
        for other in [
            "",
            "fatal: unable to access 'https://github.com/o/r.git/': Could not resolve host: github.com",
            "fatal: remote error: upload-pack: not our ref HEAD",
            "fatal: remote error: upload-pack: not our ref",
            "fatal: remote error: upload-pack: not our ref 12345",
            "fatal: remote error: upload-pack: not our ref 1234567890123456789012345678901234567891 and so on",
            "fatal: could not fetch 1234567890123456789012345678901234567891 from promisor remote",
        ] {
            assert!(refused_oids(other).is_empty(), "{other:?}");
        }
    }

    #[test]
    fn listed_oids_keep_the_order_and_repeats_of_the_input() {
        let oids = [A, B, C, B, A, C, B].map(String::from);
        // In any order, with repeats; objects that are not asked for do not matter.
        let listing = format!("{C}\n{A}\n{C}\n{}\n{A}\n", "9".repeat(40));
        assert_eq!(
            listed_oids(&oids, listing.as_bytes()).unwrap(),
            [A, C, A, C].map(String::from)
        );
        assert_eq!(
            listed_oids(&oids, format!("{B}\n{A}\n{C}").as_bytes()).unwrap(),
            oids
        );
        assert!(listed_oids(&oids, b"").unwrap().is_empty());
        assert!(listed_oids(&[], listing.as_bytes()).unwrap().is_empty());
        assert!(
            listed_oids(&oids, format!("{}\n", "9".repeat(40)).as_bytes())
                .unwrap()
                .is_empty()
        );
        // Any line that is no OID means the output is not what we think it is.
        for bad in [
            "\n".to_string(),
            "garbage\n".to_string(),
            format!("{A} missing\n"),
            format!("{A}\n\n{B}\n"),
            format!("{A}\n{}\n", "B".repeat(40)),
        ] {
            assert!(listed_oids(&oids, bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert!(listed_oids(&oids, b"\xff\n").is_err());
    }
}
