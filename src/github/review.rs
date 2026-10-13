//! Review threads and the viewer's pending review (GraphQL).

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Github, GqlPageInfo, check_oid};
use crate::error::AppError;

// GraphQL rejects unused fragments, so each query concatenates exactly the ones it spreads.
macro_rules! comment_fragment {
    () => {
        r#"
fragment Comment on PullRequestReviewComment {
  id author { login avatarUrl(size: 64) } body bodyHTML createdAt lastEditedAt url state
  pullRequestReview { id } replyTo { id } isMinimized minimizedReason
  viewerCanUpdate viewerCanDelete viewerDidAuthor
}
"#
    };
}

macro_rules! thread_fragment {
    () => {
        r#"
fragment Thread on PullRequestReviewThread {
  id path subjectType diffSide startDiffSide line startLine originalLine originalStartLine
  isResolved isOutdated isCollapsed resolvedBy { login }
  viewerCanReply viewerCanResolve viewerCanUnresolve
  hunk: comments(first: 1) { nodes { diffHunk } }
  comments(first: 100) { pageInfo { hasNextPage endCursor } nodes { ...Comment } }
}
"#
    };
}

macro_rules! pending_review_fragment {
    () => {
        r#"
fragment PendingReview on PullRequestReview {
  id body createdAt url viewerDidAuthor commit { oid }
}
"#
    };
}

const THREADS_QUERY: &str = concat!(
    r#"
query($owner: String!, $repo: String!, $number: Int!, $cursor: String, $first: Boolean!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      headRefOid
      locked @include(if: $first)
      reviews(states: [PENDING], first: 5) @include(if: $first) { nodes { ...PendingReview } }
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { ...Thread }
      }
    }
  }
}
"#,
    thread_fragment!(),
    comment_fragment!(),
    pending_review_fragment!()
);

const THREAD_COMMENTS_QUERY: &str = concat!(
    r#"
query($id: ID!, $cursor: String) {
  node(id: $id) {
    ... on PullRequestReviewThread {
      comments(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { ...Comment } }
    }
  }
}
"#,
    comment_fragment!()
);

/// Everything the diff view needs about review comments, read fresh from GitHub.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThreads {
    /// The PR head when the threads were read; line numbers refer to it.
    pub head: String,
    /// A locked conversation: GitHub refuses comments from non-maintainers.
    pub locked: bool,
    pub pending_review: Option<PendingReview>,
    pub threads: Vec<ReviewThread>,
}

/// The viewer's own PENDING review (GitHub allows one per PR and shows nobody else's).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReview {
    pub id: String,
    pub body: String,
    /// The commit the review was started on (it keeps it when the PR moves on).
    pub commit: Option<String>,
    pub created_at: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    /// "LINE" or "FILE".
    pub subject_type: String,
    /// "LEFT" (old file) or "RIGHT" (new file); `None` for file-level threads.
    pub side: Option<String>,
    /// Set only for multi-line threads.
    pub start_side: Option<String>,
    /// Line in the whole file on `side` at the current head; `None` when outdated.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub original_start_line: Option<u32>,
    /// The diff hunk GitHub showed when the thread was started (plain text).
    pub diff_hunk: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub is_collapsed: bool,
    pub resolved_by: Option<String>,
    pub viewer_can_reply: bool,
    pub viewer_can_resolve: bool,
    pub viewer_can_unresolve: bool,
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub id: String,
    /// `None` for a deleted account ("ghost").
    pub author: Option<Author>,
    pub body: String,
    /// GitHub-rendered HTML; the client sanitizes it before display.
    pub body_html: String,
    pub created_at: String,
    pub last_edited_at: Option<String>,
    pub url: String,
    /// "PENDING" (in the viewer's unsubmitted review) or "SUBMITTED".
    pub state: String,
    pub review_id: Option<String>,
    pub reply_to: Option<String>,
    pub is_minimized: bool,
    pub minimized_reason: Option<String>,
    pub viewer_can_update: bool,
    pub viewer_can_delete: bool,
    pub viewer_did_author: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Author {
    pub login: String,
    pub avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct ThreadsData {
    repository: Option<ThreadsRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsRepository {
    pull_request: Option<ThreadsPr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPr {
    head_ref_oid: String,
    #[serde(default)]
    locked: Option<bool>,
    #[serde(default)]
    reviews: Option<Nodes<GqlPendingReview>>,
    review_threads: Page<GqlThread>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Option<Vec<Option<T>>>,
}

impl<T> Nodes<T> {
    fn into_vec(self) -> Vec<T> {
        self.nodes
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    page_info: GqlPageInfo,
    nodes: Option<Vec<Option<T>>>,
}

impl<T> Page<T> {
    fn next_cursor(&self) -> Option<String> {
        self.page_info
            .has_next_page
            .then(|| self.page_info.end_cursor.clone())
            .flatten()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPendingReview {
    id: String,
    body: String,
    created_at: String,
    url: String,
    viewer_did_author: bool,
    commit: Option<GqlCommit>,
}

#[derive(Deserialize)]
struct GqlCommit {
    oid: String,
}

#[derive(Deserialize)]
struct GqlLogin {
    login: String,
}

#[derive(Deserialize)]
struct GqlId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlHunk {
    diff_hunk: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThread {
    id: String,
    path: String,
    subject_type: Option<String>,
    diff_side: Option<String>,
    start_diff_side: Option<String>,
    line: Option<u32>,
    start_line: Option<u32>,
    original_line: Option<u32>,
    original_start_line: Option<u32>,
    is_resolved: bool,
    is_outdated: bool,
    is_collapsed: bool,
    resolved_by: Option<GqlLogin>,
    viewer_can_reply: bool,
    viewer_can_resolve: bool,
    viewer_can_unresolve: bool,
    hunk: Nodes<GqlHunk>,
    comments: Page<GqlComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlComment {
    id: String,
    author: Option<Author>,
    body: String,
    #[serde(rename = "bodyHTML")]
    body_html: String,
    created_at: String,
    last_edited_at: Option<String>,
    url: String,
    state: String,
    pull_request_review: Option<GqlId>,
    reply_to: Option<GqlId>,
    is_minimized: bool,
    minimized_reason: Option<String>,
    viewer_can_update: bool,
    viewer_can_delete: bool,
    viewer_did_author: bool,
}

#[derive(Deserialize)]
struct ThreadNodeData {
    node: Option<ThreadNode>,
}

#[derive(Deserialize)]
struct ThreadNode {
    comments: Option<Page<GqlComment>>,
}

fn side(s: Option<String>) -> Option<String> {
    s.filter(|s| s == "LEFT" || s == "RIGHT")
}

/// A range start only counts with its own side, and on the same side it must come first.
/// (GitHub returns `startLine == line` with no start side for single-line threads, and odd
/// values for some.)
fn range_start(
    start: Option<u32>,
    start_side: &Option<String>,
    end: Option<u32>,
    end_side: &Option<String>,
) -> Option<u32> {
    let (start, start_side) = (start?, start_side.as_ref()?);
    match end {
        Some(end) if Some(start_side) == end_side.as_ref() && start >= end => None,
        _ => Some(start),
    }
}

impl GqlComment {
    fn into_api(self) -> ReviewComment {
        ReviewComment {
            id: self.id,
            author: self.author,
            body: self.body,
            body_html: self.body_html,
            created_at: self.created_at,
            last_edited_at: self.last_edited_at,
            url: self.url,
            state: self.state,
            review_id: self.pull_request_review.map(|r| r.id),
            reply_to: self.reply_to.map(|r| r.id),
            is_minimized: self.is_minimized,
            minimized_reason: self.minimized_reason,
            viewer_can_update: self.viewer_can_update,
            viewer_can_delete: self.viewer_can_delete,
            viewer_did_author: self.viewer_did_author,
        }
    }
}

impl GqlThread {
    /// Normalizes GitHub's quirks; `None` for a thread without comments (seen briefly after
    /// its pending review was deleted).
    fn into_api(self, extra: Vec<GqlComment>) -> Option<ReviewThread> {
        let comments: Vec<ReviewComment> = self
            .comments
            .nodes
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .chain(extra)
            .map(GqlComment::into_api)
            .collect();
        if comments.is_empty() {
            return None;
        }
        let file_level = self.subject_type.as_deref() == Some("FILE");
        let (diff_side, start_side, line, start_line, original_line, original_start_line) =
            if file_level {
                (None, None, None, None, None, None)
            } else {
                let diff_side = side(self.diff_side);
                let start_side = side(self.start_diff_side);
                let start_line = range_start(self.start_line, &start_side, self.line, &diff_side);
                let original_start_line = range_start(
                    self.original_start_line,
                    &start_side,
                    self.original_line,
                    &diff_side,
                );
                let start_side =
                    start_side.filter(|_| start_line.or(original_start_line).is_some());
                (
                    diff_side,
                    start_side,
                    self.line,
                    start_line,
                    self.original_line,
                    original_start_line,
                )
            };
        Some(ReviewThread {
            id: self.id,
            path: self.path,
            subject_type: if file_level { "FILE" } else { "LINE" }.to_string(),
            side: diff_side,
            start_side,
            line,
            start_line,
            original_line,
            original_start_line,
            diff_hunk: self
                .hunk
                .into_vec()
                .into_iter()
                .next()
                .map(|h| h.diff_hunk)
                .unwrap_or_default(),
            is_resolved: self.is_resolved,
            is_outdated: self.is_outdated,
            is_collapsed: self.is_collapsed,
            resolved_by: self.resolved_by.map(|r| r.login),
            viewer_can_reply: self.viewer_can_reply,
            viewer_can_resolve: self.viewer_can_resolve,
            viewer_can_unresolve: self.viewer_can_unresolve,
            comments,
        })
    }
}

fn pending_review(reviews: Option<Nodes<GqlPendingReview>>) -> Option<PendingReview> {
    reviews?
        .into_vec()
        .into_iter()
        .find(|r| r.viewer_did_author)
        .map(|r| PendingReview {
            id: r.id,
            body: r.body,
            commit: r.commit.map(|c| c.oid).filter(|oid| check_oid(oid).is_ok()),
            created_at: r.created_at,
            url: r.url,
        })
}

impl Github {
    /// Every review thread of the PR (all pages, all comments) plus the viewer's pending
    /// review and the head the line numbers refer to.
    pub async fn review_threads(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<ReviewThreads, AppError> {
        let mut cursor: Option<String> = None;
        let mut head = String::new();
        let mut locked = false;
        let mut pending = None;
        let mut raw: Vec<GqlThread> = Vec::new();
        loop {
            let first = cursor.is_none();
            let data: ThreadsData = self
                .graphql(
                    host,
                    token,
                    THREADS_QUERY,
                    json!({ "owner": owner, "repo": repo, "number": number,
                            "cursor": cursor, "first": first }),
                )
                .await?;
            let pr = data
                .repository
                .and_then(|r| r.pull_request)
                .ok_or_else(|| {
                    AppError::not_found(format!("pull request {owner}/{repo}#{number} not found"))
                })?;
            if first {
                check_oid(&pr.head_ref_oid)?;
                head = pr.head_ref_oid;
                locked = pr.locked.unwrap_or(false);
                pending = pending_review(pr.reviews);
            }
            let next = pr.review_threads.next_cursor();
            raw.extend(
                pr.review_threads
                    .nodes
                    .unwrap_or_default()
                    .into_iter()
                    .flatten(),
            );
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        let mut threads = Vec::with_capacity(raw.len());
        for t in raw {
            let extra = match t.comments.next_cursor() {
                Some(c) => self.more_comments(host, token, &t.id, c).await?,
                None => Vec::new(),
            };
            threads.extend(t.into_api(extra));
        }
        Ok(ReviewThreads {
            head,
            locked,
            pending_review: pending,
            threads,
        })
    }

    /// The comments of a thread after `cursor` (threads with more than 100 comments).
    async fn more_comments(
        &self,
        host: &str,
        token: &str,
        thread_id: &str,
        mut cursor: String,
    ) -> Result<Vec<GqlComment>, AppError> {
        let mut out = Vec::new();
        loop {
            let data: ThreadNodeData = self
                .graphql(
                    host,
                    token,
                    THREAD_COMMENTS_QUERY,
                    json!({ "id": thread_id, "cursor": cursor }),
                )
                .await?;
            let Some(page) = data.node.and_then(|n| n.comments) else {
                break;
            };
            let next = page.next_cursor();
            out.extend(page.nodes.unwrap_or_default().into_iter().flatten());
            match next {
                Some(c) => cursor = c,
                None => break,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::GqlResponse;

    const HEAD: &str = "2222222222222222222222222222222222222222";

    fn comment(id: &str, state: &str, author: &str) -> String {
        let author = if author.is_empty() {
            "null".to_string()
        } else {
            format!(
                r#"{{"login": "{author}", "avatarUrl": "https://avatars.githubusercontent.com/u/1?s=64"}}"#
            )
        };
        format!(
            r#"{{"id": "{id}", "author": {author}, "body": "b {id}", "bodyHTML": "<p>b {id}</p>",
                "createdAt": "2026-10-13T00:00:00Z", "lastEditedAt": null, "url": "https://github.com/o/r/pull/1#discussion_r1",
                "state": "{state}", "pullRequestReview": {{"id": "PRR_1"}}, "replyTo": null,
                "isMinimized": false, "minimizedReason": null,
                "viewerCanUpdate": true, "viewerCanDelete": true, "viewerDidAuthor": true}}"#
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn thread(
        id: &str,
        subject: &str,
        side: &str,
        start_side: &str,
        line: &str,
        start_line: &str,
        comments: &[String],
        more: bool,
    ) -> String {
        let q = |s: &str| {
            if s == "null" {
                s.to_string()
            } else {
                format!("\"{s}\"")
            }
        };
        format!(
            r#"{{"id": "{id}", "path": "src/a.rs", "subjectType": "{subject}",
                "diffSide": {}, "startDiffSide": {}, "line": {line}, "startLine": {start_line},
                "originalLine": {line}, "originalStartLine": {start_line},
                "isResolved": false, "isOutdated": false, "isCollapsed": false, "resolvedBy": null,
                "viewerCanReply": true, "viewerCanResolve": true, "viewerCanUnresolve": false,
                "hunk": {{"nodes": [{{"diffHunk": "@@ -1,3 +1,4 @@"}}]}},
                "comments": {{"pageInfo": {{"hasNextPage": {more}, "endCursor": "c1"}}, "nodes": [{}]}}}}"#,
            q(side),
            q(start_side),
            comments.join(",")
        )
    }

    fn parse(threads: &[String], pending: &str) -> ThreadsPr {
        let body = format!(
            r#"{{"data": {{"repository": {{"pullRequest": {{
                "headRefOid": "{HEAD}", "locked": false,
                "reviews": {{"nodes": [{pending}]}},
                "reviewThreads": {{"pageInfo": {{"hasNextPage": false, "endCursor": null}}, "nodes": [{}]}}
            }}}}}}}}"#,
            threads.join(",")
        );
        let resp: GqlResponse<ThreadsData> = serde_json::from_str(&body).unwrap();
        resp.data.unwrap().repository.unwrap().pull_request.unwrap()
    }

    fn normalize(pr: ThreadsPr) -> Vec<ReviewThread> {
        pr.review_threads
            .nodes
            .unwrap()
            .into_iter()
            .flatten()
            .filter_map(|t| t.into_api(Vec::new()))
            .collect()
    }

    #[test]
    fn threads_are_normalized() {
        let c = |id: &str| comment(id, "SUBMITTED", "alice");
        let pr = parse(
            &[
                // Single line: GitHub reports startLine == line and no start side.
                thread(
                    "T1",
                    "LINE",
                    "RIGHT",
                    "null",
                    "110",
                    "110",
                    &[c("C1")],
                    false,
                ),
                // A real range.
                thread(
                    "T2",
                    "LINE",
                    "RIGHT",
                    "RIGHT",
                    "111",
                    "108",
                    &[c("C2")],
                    false,
                ),
                // A range across sides.
                thread("T3", "LINE", "RIGHT", "LEFT", "2", "1", &[c("C3")], false),
                // Garbage start without a side (seen on an old rename path).
                thread("T4", "LINE", "LEFT", "null", "1", "2", &[c("C4")], false),
                // File level: GitHub reports line 1 on RIGHT.
                thread(
                    "T5",
                    "FILE",
                    "RIGHT",
                    "null",
                    "1",
                    "null",
                    &[c("C5")],
                    false,
                ),
                // Outdated: no current line.
                thread(
                    "T6",
                    "LINE",
                    "RIGHT",
                    "null",
                    "null",
                    "null",
                    &[c("C6")],
                    false,
                ),
                // No comments (a deleted pending review, briefly): dropped.
                thread("T7", "LINE", "RIGHT", "null", "5", "5", &[], false),
            ],
            "",
        );
        let t = normalize(pr);
        let ids: Vec<&str> = t.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["T1", "T2", "T3", "T4", "T5", "T6"]);
        fn shape(t: &ReviewThread) -> (&str, Option<&str>, Option<&str>, Option<u32>, Option<u32>) {
            (
                t.subject_type.as_str(),
                t.side.as_deref(),
                t.start_side.as_deref(),
                t.line,
                t.start_line,
            )
        }
        assert_eq!(shape(&t[0]), ("LINE", Some("RIGHT"), None, Some(110), None));
        assert_eq!(
            shape(&t[1]),
            ("LINE", Some("RIGHT"), Some("RIGHT"), Some(111), Some(108))
        );
        assert_eq!(
            shape(&t[2]),
            ("LINE", Some("RIGHT"), Some("LEFT"), Some(2), Some(1))
        );
        assert_eq!(shape(&t[3]), ("LINE", Some("LEFT"), None, Some(1), None));
        assert_eq!(shape(&t[4]), ("FILE", None, None, None, None));
        assert_eq!(t[4].original_line, None);
        assert_eq!(shape(&t[5]), ("LINE", Some("RIGHT"), None, None, None));
        assert_eq!(t[0].diff_hunk, "@@ -1,3 +1,4 @@");
        assert_eq!(t[0].comments[0].body_html, "<p>b C1</p>");
        assert_eq!(t[0].comments[0].review_id.as_deref(), Some("PRR_1"));
    }

    #[test]
    fn ghost_authors_pending_comments_and_unknown_sides() {
        let pr = parse(
            &[thread(
                "T1",
                "LINE",
                "MIDDLE",
                "null",
                "3",
                "null",
                &[
                    comment("C1", "SUBMITTED", ""),
                    comment("C2", "PENDING", "me"),
                ],
                false,
            )],
            "",
        );
        let t = normalize(pr);
        assert!(t[0].comments[0].author.is_none());
        assert_eq!(t[0].comments[1].state, "PENDING");
        assert_eq!(
            t[0].comments[1].author.as_ref().map(|a| a.login.as_str()),
            Some("me")
        );
        assert_eq!(t[0].side, None);
    }

    #[test]
    fn extra_comment_pages_are_appended() {
        let pr = parse(
            &[thread(
                "T1",
                "LINE",
                "RIGHT",
                "null",
                "3",
                "3",
                &[comment("C1", "SUBMITTED", "a")],
                true,
            )],
            "",
        );
        let raw = pr
            .review_threads
            .nodes
            .unwrap()
            .into_iter()
            .flatten()
            .next()
            .unwrap();
        assert_eq!(raw.comments.next_cursor().as_deref(), Some("c1"));
        let extra: GqlComment = serde_json::from_str(&comment("C2", "SUBMITTED", "b")).unwrap();
        let t = raw.into_api(vec![extra]).unwrap();
        let ids: Vec<&str> = t.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["C1", "C2"]);
    }

    #[test]
    fn only_the_viewers_pending_review_counts() {
        let review = |id: &str, mine: bool, commit: &str| {
            format!(
                r#"{{"id": "{id}", "body": "", "createdAt": "2026-10-13T00:00:00Z", "url": "u",
                    "viewerDidAuthor": {mine}, "commit": {{"oid": "{commit}"}}}}"#
            )
        };
        let pr = parse(
            &[],
            &format!("{},{}", review("R1", false, HEAD), review("R2", true, HEAD)),
        );
        let p = pending_review(pr.reviews).unwrap();
        assert_eq!(p.id, "R2");
        assert_eq!(p.commit.as_deref(), Some(HEAD));
        // An invalid commit id is dropped rather than passed on.
        let pr = parse(&[], &review("R3", true, "not-an-oid"));
        assert_eq!(pending_review(pr.reviews).unwrap().commit, None);
        // Later pages don't ask for reviews or `locked` at all.
        let body = r#"{"data": {"repository": {"pullRequest": {"headRefOid": "2222222222222222222222222222222222222222",
            "reviewThreads": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}}}}}"#;
        let resp: GqlResponse<ThreadsData> = serde_json::from_str(body).unwrap();
        let pr = resp.data.unwrap().repository.unwrap().pull_request.unwrap();
        assert!(pr.locked.is_none() && pr.reviews.is_none());
    }

    /// GraphQL rejects a query with an undefined or an unused fragment.
    #[test]
    fn queries_define_exactly_the_fragments_they_spread() {
        for q in [THREADS_QUERY, THREAD_COMMENTS_QUERY] {
            for name in ["Comment", "Thread", "PendingReview"] {
                let spread =
                    q.contains(&format!("...{name} ")) || q.contains(&format!("...{name}\n"));
                let defined = q.contains(&format!("fragment {name} on"));
                assert_eq!(spread, defined, "{name} in {q}");
            }
        }
    }
}
