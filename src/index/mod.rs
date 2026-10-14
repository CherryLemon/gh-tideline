//! A symbol index for code navigation: where names are defined and where they are used.
//!
//! The unit of work is a blob (the content of a file, named by its git OID). Each blob is parsed once with tree-sitter
//! into tags, the definitions and references of names it contains (`tags`), and every commit that contains the blob
//! shares those tags. A commit itself is only a listing of the blobs at its paths, its tree, so indexing a new commit
//! parses just the blobs not seen before. Lookups find tags by name within a language family (`lang`): a TypeScript
//! name sees JavaScript and TSX files, a C name sees C++ files, and no other language is mixed.
//!
//! The tags and the commits are kept in a SQLite file (`db`), which also answers the lookups. Their results are
//! ordered by how close they are to the file the lookup came from (`rank`).

pub mod db;
pub mod lang;
mod rank;
pub mod tags;
