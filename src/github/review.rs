//! Review threads and the viewer's pending review (GraphQL).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Github, GqlPageInfo, check_oid};
use crate::error::{AppError, ErrorCode};

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
  id body createdAt url viewerDidAuthor commit { oid } commentCount: comments { totalCount }
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
      viewerLatestReview @include(if: $first) { id state url body }
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

const REVIEW_STATE_QUERY: &str = concat!(
    r#"
query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id headRefOid
      reviews(states: [PENDING], first: 5) { nodes { ...PendingReview } }
    }
  }
}
"#,
    pending_review_fragment!()
);

const THREAD_QUERY: &str = concat!(
    r#"
query($id: ID!) {
  node(id: $id) { __typename ...Thread }
}
"#,
    thread_fragment!(),
    comment_fragment!()
);

const ADD_REVIEW: &str = concat!(
    r#"
mutation($pr: ID!, $commit: GitObjectID!, $event: PullRequestReviewEvent,
         $threads: [DraftPullRequestReviewThread]) {
  addPullRequestReview(input: {pullRequestId: $pr, commitOID: $commit, event: $event,
                               threads: $threads}) {
    pullRequestReview { ...PendingReview comments(first: 1) { nodes { id } } }
  }
}
"#,
    pending_review_fragment!()
);

const ADD_THREAD: &str = concat!(
    r#"
mutation($review: ID!, $path: String!, $body: String!,
         $subjectType: PullRequestReviewThreadSubjectType, $line: Int, $side: DiffSide,
         $startLine: Int, $startSide: DiffSide) {
  addPullRequestReviewThread(input: {pullRequestReviewId: $review, path: $path, body: $body,
                                     subjectType: $subjectType, line: $line, side: $side,
                                     startLine: $startLine, startSide: $startSide}) {
    thread { ...Thread }
  }
}
"#,
    thread_fragment!(),
    comment_fragment!()
);

const SUBMIT_REVIEW: &str = r#"
mutation($review: ID!, $event: PullRequestReviewEvent!, $body: String) {
  submitPullRequestReview(input: {pullRequestReviewId: $review, event: $event, body: $body}) {
    pullRequestReview { id state url body }
  }
}
"#;

/// A review of just a summary, submitted in the same call.
const SUBMIT_NEW_REVIEW: &str = r#"
mutation($pr: ID!, $commit: GitObjectID!, $event: PullRequestReviewEvent!, $body: String) {
  addPullRequestReview(input: {pullRequestId: $pr, commitOID: $commit, event: $event,
                               body: $body}) {
    pullRequestReview { id state url body }
  }
}
"#;

const DELETE_REVIEW: &str = r#"
mutation($review: ID!) {
  deletePullRequestReview(input: {pullRequestReviewId: $review}) { pullRequestReview { id } }
}
"#;

const ADD_REPLY: &str = concat!(
    r#"
mutation($thread: ID!, $review: ID, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread,
                                          pullRequestReviewId: $review, body: $body}) {
    comment { ...Comment }
  }
}
"#,
    comment_fragment!()
);

const RESOLVE_THREAD: &str = concat!(
    r#"
mutation($thread: ID!) {
  resolveReviewThread(input: {threadId: $thread}) { thread { ...Thread } }
}
"#,
    thread_fragment!(),
    comment_fragment!()
);

const UNRESOLVE_THREAD: &str = concat!(
    r#"
mutation($thread: ID!) {
  unresolveReviewThread(input: {threadId: $thread}) { thread { ...Thread } }
}
"#,
    thread_fragment!(),
    comment_fragment!()
);

const UPDATE_COMMENT: &str = concat!(
    r#"
mutation($comment: ID!, $body: String!) {
  updatePullRequestReviewComment(input: {pullRequestReviewCommentId: $comment, body: $body}) {
    pullRequestReviewComment { ...Comment }
  }
}
"#,
    comment_fragment!()
);

const DELETE_COMMENT: &str = r#"
mutation($comment: ID!) {
  deletePullRequestReviewComment(input: {id: $comment}) { pullRequestReview { id } }
}
"#;

/// What GitHub says when it does not take a line for a comment (a null thread, no error).
const LINE_REJECTED: &str =
    "GitHub only takes comments on lines in the diff, or within 3 lines of a change";

/// Everything the diff view needs about review comments, read fresh from GitHub.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThreads {
    /// The PR head when the threads were read; line numbers refer to it.
    pub head: String,
    /// A locked conversation: GitHub refuses comments from non-maintainers.
    pub locked: bool,
    pub pending_review: Option<PendingReview>,
    /// The viewer's latest submitted review (never a pending one): a submit whose answer was
    /// lost shows here.
    pub latest_review: Option<SubmittedReview>,
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
    /// GitHub refuses to submit an empty review without a summary (and doesn't say why).
    #[serde(skip_serializing)]
    pub comment_count: u32,
}

/// A review as submitted: `state` is GitHub's (`COMMENTED`, `APPROVED`, `CHANGES_REQUESTED`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubmittedReview {
    pub id: String,
    pub state: String,
    pub url: String,
    /// The summary (Markdown; empty for the review a single comment makes): with `state`, how a
    /// summary-only submit whose answer was lost is told apart from other reviews.
    pub body: String,
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

/// What every write starts from, read fresh: the PR's node id, its head and the viewer's
/// pending review.
#[derive(Debug, Clone)]
pub struct ReviewState {
    pub pr_id: String,
    pub head: String,
    pub pending: Option<PendingReview>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DiffSide {
    Left,
    Right,
}

impl DiffSide {
    fn as_str(self) -> &'static str {
        match self {
            DiffSide::Left => "LEFT",
            DiffSide::Right => "RIGHT",
        }
    }
}

/// What a new thread is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadTarget {
    File,
    /// `line`, or `start_line..=line`, all on `side`.
    Lines {
        side: DiffSide,
        line: u32,
        start_line: Option<u32>,
    },
}

/// A thread to start, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewThread {
    pub path: String,
    pub body: String,
    pub target: ThreadTarget,
}

impl NewThread {
    /// The line fields as GraphQL variables (null for a file).
    fn line_fields(&self) -> serde_json::Value {
        match &self.target {
            ThreadTarget::File => json!({ "line": null, "side": null, "startLine": null,
                                          "startSide": null }),
            ThreadTarget::Lines {
                side,
                line,
                start_line,
            } => json!({
                "line": line,
                "side": side.as_str(),
                "startLine": start_line,
                "startSide": start_line.map(|_| side.as_str()),
            }),
        }
    }
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
    #[serde(default)]
    viewer_latest_review: Option<SubmittedReview>,
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
    #[serde(default)]
    comment_count: Option<GqlCount>,
}

#[derive(Deserialize)]
struct GqlCommit {
    oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlCount {
    total_count: u32,
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

#[derive(Deserialize)]
struct StateData {
    repository: Option<StateRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateRepository {
    pull_request: Option<StatePr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatePr {
    id: String,
    head_ref_oid: String,
    reviews: Option<Nodes<GqlPendingReview>>,
}

#[derive(Deserialize)]
struct NodeData {
    node: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddReviewData {
    add_pull_request_review: Option<AddReviewPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddReviewPayload {
    pull_request_review: Option<GqlAddedReview>,
}

#[derive(Deserialize)]
struct GqlAddedReview {
    #[serde(flatten)]
    review: GqlPendingReview,
    comments: Nodes<GqlId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddThreadData {
    add_pull_request_review_thread: Option<ThreadPayload>,
}

#[derive(Deserialize)]
struct ThreadPayload {
    thread: Option<GqlThread>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitData {
    // A review submitted on its own, or added already submitted.
    #[serde(alias = "addPullRequestReview")]
    submit_pull_request_review: Option<SubmitPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitPayload {
    pull_request_review: Option<SubmittedReview>,
}

impl SubmitData {
    /// `None`: GitHub took it (no error) but did not return the review.
    fn review(self) -> Option<SubmittedReview> {
        self.submit_pull_request_review
            .and_then(|p| p.pull_request_review)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddReplyData {
    add_pull_request_review_thread_reply: Option<CommentPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCommentData {
    update_pull_request_review_comment: Option<UpdatedComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatedComment {
    pull_request_review_comment: Option<GqlComment>,
}

#[derive(Deserialize)]
struct CommentPayload {
    comment: Option<GqlComment>,
}

/// `resolveReviewThread` and `unresolveReviewThread` answer alike.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveData {
    #[serde(alias = "unresolveReviewThread")]
    resolve_review_thread: Option<ThreadPayload>,
}

/// GitHub's own words for lines it does not take in a review ("Line could not be resolved"),
/// said the way `add_thread` says them.
fn line_rejection(e: AppError) -> AppError {
    if e.code == ErrorCode::Rejected && e.message.contains("could not be resolved") {
        AppError::rejected(LINE_REJECTED)
    } else {
        e
    }
}

/// The thread of a `node(id:)` query; `None` for no node, or a node of another type.
fn thread_node(node: Option<serde_json::Value>) -> Result<Option<GqlThread>, AppError> {
    let Some(node) = node.filter(|n| n["__typename"] == "PullRequestReviewThread") else {
        return Ok(None);
    };
    serde_json::from_value(node)
        .map(Some)
        .map_err(|e| AppError::github(format!("unexpected review thread: {e}")))
}

/// The thread `addPullRequestReviewThread` created; no thread means GitHub did not take it.
/// `None`: it was created, but came back without its comment (read it again).
fn added_thread(
    data: AddThreadData,
    target: &ThreadTarget,
) -> Result<Option<ReviewThread>, AppError> {
    let Some(added) = data.add_pull_request_review_thread.and_then(|p| p.thread) else {
        return Err(AppError::rejected(match target {
            ThreadTarget::File => "GitHub did not take a comment on this file",
            ThreadTarget::Lines { .. } => LINE_REJECTED,
        }));
    };
    let id = added.id.clone();
    let thread = added.into_api(Vec::new());
    if thread.is_none() {
        eprintln!("gh-tideline: GitHub returned the new thread {id} without its comment");
    }
    Ok(thread)
}

fn side(s: Option<String>) -> Option<String> {
    s.filter(|s| s == "LEFT" || s == "RIGHT")
}

/// A range start only counts with its own side and an end, and on the same side it must come
/// first.
/// (GitHub returns `startLine == line` with no start side for single-line threads, and odd
/// values for some.)
fn range_start(
    start: Option<u32>,
    start_side: &Option<String>,
    end: Option<u32>,
    end_side: &Option<String>,
) -> Option<u32> {
    let (start, start_side, end) = (start?, start_side.as_ref()?, end?);
    if Some(start_side) == end_side.as_ref() && start >= end {
        None
    } else {
        Some(start)
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
            // GitHub's enum has only these two; anything new is treated as posted.
            state: if self.state == "PENDING" {
                "PENDING"
            } else {
                "SUBMITTED"
            }
            .to_string(),
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

impl GqlPendingReview {
    fn into_api(self) -> PendingReview {
        PendingReview {
            id: self.id,
            body: self.body,
            commit: self
                .commit
                .map(|c| c.oid)
                .filter(|oid| check_oid(oid).is_ok()),
            created_at: self.created_at,
            url: self.url,
            comment_count: self.comment_count.map_or(0, |c| c.total_count),
        }
    }
}

fn pending_review(reviews: Option<Nodes<GqlPendingReview>>) -> Option<PendingReview> {
    reviews?
        .into_vec()
        .into_iter()
        .find(|r| r.viewer_did_author)
        .map(GqlPendingReview::into_api)
}

/// Reads of a PR whose head moves between thread pages are retried this many times in total.
const THREAD_READ_ATTEMPTS: usize = 3;

/// The thread pages read so far.
#[derive(Default)]
struct ThreadPages {
    head: String,
    locked: bool,
    pending: Option<PendingReview>,
    latest: Option<SubmittedReview>,
    raw: Vec<GqlThread>,
    /// A thread added or removed between pages shifts the rest, so one can come twice.
    seen: HashSet<String>,
}

enum PageEnd {
    Next(String),
    Done,
    /// Line numbers on this page refer to a different commit than the first page's.
    HeadMoved,
}

impl ThreadPages {
    fn add(&mut self, pr: ThreadsPr, first: bool) -> Result<PageEnd, AppError> {
        if first {
            check_oid(&pr.head_ref_oid)?;
            self.head = pr.head_ref_oid;
            self.locked = pr.locked.unwrap_or(false);
            self.pending = pending_review(pr.reviews);
            self.latest = pr.viewer_latest_review;
        } else if pr.head_ref_oid != self.head {
            return Ok(PageEnd::HeadMoved);
        }
        let next = pr.review_threads.next_cursor();
        let seen = &mut self.seen;
        self.raw.extend(
            pr.review_threads
                .nodes
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .filter(|t| seen.insert(t.id.clone())),
        );
        Ok(next.map_or(PageEnd::Done, PageEnd::Next))
    }
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
        for _ in 0..THREAD_READ_ATTEMPTS {
            if let Some(threads) = self
                .review_threads_once(host, token, owner, repo, number)
                .await?
            {
                return Ok(threads);
            }
        }
        Err(AppError::github(
            "the pull request kept changing while its review threads were read; try again",
        ))
    }

    /// One read of all thread pages; `None` if the head moved between pages, since line
    /// numbers on the later pages would then refer to a different commit.
    async fn review_threads_once(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Option<ReviewThreads>, AppError> {
        let mut pages = ThreadPages::default();
        let mut cursor: Option<String> = None;
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
            match pages.add(pr, first)? {
                PageEnd::Next(c) => cursor = Some(c),
                PageEnd::Done => break,
                PageEnd::HeadMoved => return Ok(None),
            }
        }
        let ThreadPages {
            head,
            locked,
            pending,
            latest,
            raw,
            ..
        } = pages;
        let mut threads = Vec::with_capacity(raw.len());
        for t in raw {
            let extra = match t.comments.next_cursor() {
                Some(c) => match self.more_comments(host, token, &t.id, c).await {
                    Ok(extra) => extra,
                    // Deleted since the first query.
                    Err(e) if e.code == ErrorCode::NotFound => continue,
                    Err(e) => return Err(e),
                },
                None => Vec::new(),
            };
            threads.extend(t.into_api(extra));
        }
        Ok(Some(ReviewThreads {
            head,
            locked,
            pending_review: pending,
            latest_review: latest,
            threads,
        }))
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

impl Github {
    /// The PR's node id, head and the viewer's pending review, read fresh before a write.
    pub async fn review_state(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<ReviewState, AppError> {
        let data: StateData = self
            .graphql(
                host,
                token,
                REVIEW_STATE_QUERY,
                json!({ "owner": owner, "repo": repo, "number": number }),
            )
            .await?;
        let pr = data
            .repository
            .and_then(|r| r.pull_request)
            .ok_or_else(|| {
                AppError::not_found(format!("pull request {owner}/{repo}#{number} not found"))
            })?;
        check_oid(&pr.head_ref_oid)?;
        Ok(ReviewState {
            pr_id: pr.id,
            head: pr.head_ref_oid,
            pending: pending_review(pr.reviews),
        })
    }

    /// One review thread with all its comments; `None` once it is gone.
    pub async fn review_thread(
        &self,
        host: &str,
        token: &str,
        id: &str,
    ) -> Result<Option<ReviewThread>, AppError> {
        let data: NodeData = match self
            .graphql(host, token, THREAD_QUERY, json!({ "id": id }))
            .await
        {
            Ok(data) => data,
            Err(e) if e.code == ErrorCode::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match thread_node(data.node)? {
            Some(thread) => self.full_thread(host, token, thread).await,
            None => Ok(None),
        }
    }

    /// `thread` with the comments after its first page.
    async fn full_thread(
        &self,
        host: &str,
        token: &str,
        thread: GqlThread,
    ) -> Result<Option<ReviewThread>, AppError> {
        let extra = match thread.comments.next_cursor() {
            Some(cursor) => self.more_comments(host, token, &thread.id, cursor).await?,
            None => Vec::new(),
        };
        Ok(thread.into_api(extra))
    }

    /// Starts the viewer's pending review on `commit`.
    pub async fn start_review(
        &self,
        host: &str,
        token: &str,
        pr_id: &str,
        commit: &str,
    ) -> Result<PendingReview, AppError> {
        let added = self
            .add_review(host, token, json!({ "pr": pr_id, "commit": commit }))
            .await?;
        Ok(added.review.into_api())
    }

    /// Posts a comment on lines right away, as a review of that one comment on `commit`.
    /// GitHub refuses the whole review when it does not take the lines. Returns the comment's
    /// id (GitHub does not return its thread).
    pub async fn comment_on_lines(
        &self,
        host: &str,
        token: &str,
        pr_id: &str,
        commit: &str,
        thread: &NewThread,
    ) -> Result<Option<String>, AppError> {
        if thread.target == ThreadTarget::File {
            return Err(AppError::bad_request(
                "a comment on a file needs a pending review",
            ));
        }
        let mut draft = thread.line_fields();
        draft["path"] = json!(thread.path);
        draft["body"] = json!(thread.body);
        let added = self
            .add_review(
                host,
                token,
                json!({ "pr": pr_id, "commit": commit, "event": "COMMENT", "threads": [draft] }),
            )
            .await
            .map_err(line_rejection)?;
        Ok(added.comments.into_vec().into_iter().next().map(|c| c.id))
    }

    async fn add_review(
        &self,
        host: &str,
        token: &str,
        variables: serde_json::Value,
    ) -> Result<GqlAddedReview, AppError> {
        let data: AddReviewData = self.mutate(host, token, ADD_REVIEW, variables).await?;
        data.add_pull_request_review
            .and_then(|p| p.pull_request_review)
            .ok_or_else(|| AppError::github("GitHub returned no review"))
    }

    /// Adds a thread to the pending review `review_id`. GitHub answers lines it does not take
    /// with no thread and no error; that is `rejected` here. `None`: added, but GitHub's
    /// answer lacks the thread's comment.
    pub async fn add_thread(
        &self,
        host: &str,
        token: &str,
        review_id: &str,
        thread: &NewThread,
    ) -> Result<Option<ReviewThread>, AppError> {
        let mut variables = thread.line_fields();
        variables["review"] = json!(review_id);
        variables["path"] = json!(thread.path);
        variables["body"] = json!(thread.body);
        variables["subjectType"] = json!(match thread.target {
            ThreadTarget::File => "FILE",
            ThreadTarget::Lines { .. } => "LINE",
        });
        let data: AddThreadData = self.mutate(host, token, ADD_THREAD, variables).await?;
        added_thread(data, &thread.target)
    }

    /// Submits the pending review `review_id` as `event` (COMMENT, APPROVE, REQUEST_CHANGES).
    /// `None`: submitted, but GitHub's answer lacks the review.
    pub async fn submit_review(
        &self,
        host: &str,
        token: &str,
        review_id: &str,
        event: &str,
        body: Option<&str>,
    ) -> Result<Option<SubmittedReview>, AppError> {
        let data: SubmitData = self
            .mutate(
                host,
                token,
                SUBMIT_REVIEW,
                json!({ "review": review_id, "event": event, "body": body }),
            )
            .await?;
        Ok(data.review())
    }

    /// Submits a review of just `body` (none for an approval without words) on `commit`, when
    /// the viewer has no pending review. `None` as for [`Self::submit_review`].
    pub async fn submit_new_review(
        &self,
        host: &str,
        token: &str,
        pr_id: &str,
        commit: &str,
        event: &str,
        body: Option<&str>,
    ) -> Result<Option<SubmittedReview>, AppError> {
        let variables = json!({ "pr": pr_id, "commit": commit, "event": event, "body": body });
        let data: SubmitData = self
            .mutate(host, token, SUBMIT_NEW_REVIEW, variables)
            .await?;
        Ok(data.review())
    }

    /// Deletes the pending review `review_id` with all its comments.
    pub async fn delete_review(
        &self,
        host: &str,
        token: &str,
        review_id: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .mutate(host, token, DELETE_REVIEW, json!({ "review": review_id }))
            .await?;
        Ok(())
    }

    /// Replies to `thread_id`: in the pending review `review_id`, or posted right away without
    /// one (spike S13). `None`: GitHub answered without the comment.
    pub async fn add_reply(
        &self,
        host: &str,
        token: &str,
        thread_id: &str,
        review_id: Option<&str>,
        body: &str,
    ) -> Result<Option<ReviewComment>, AppError> {
        let data: AddReplyData = self
            .mutate(
                host,
                token,
                ADD_REPLY,
                json!({ "thread": thread_id, "review": review_id, "body": body }),
            )
            .await?;
        Ok(data
            .add_pull_request_review_thread_reply
            .and_then(|p| p.comment)
            .map(GqlComment::into_api))
    }

    /// Resolves or unresolves `thread_id`; returns the thread as it is now.
    pub async fn set_resolved(
        &self,
        host: &str,
        token: &str,
        thread_id: &str,
        resolved: bool,
    ) -> Result<ReviewThread, AppError> {
        let query = if resolved {
            RESOLVE_THREAD
        } else {
            UNRESOLVE_THREAD
        };
        let data: ResolveData = self
            .mutate(host, token, query, json!({ "thread": thread_id }))
            .await?;
        let thread = match data.resolve_review_thread.and_then(|p| p.thread) {
            Some(thread) => self.full_thread(host, token, thread).await?,
            None => None,
        };
        thread.ok_or_else(|| AppError::github("GitHub did not return the thread"))
    }

    /// Replaces the body of the comment `comment_id`.
    pub async fn update_comment(
        &self,
        host: &str,
        token: &str,
        comment_id: &str,
        body: &str,
    ) -> Result<ReviewComment, AppError> {
        let data: UpdateCommentData = self
            .mutate(
                host,
                token,
                UPDATE_COMMENT,
                json!({ "comment": comment_id, "body": body }),
            )
            .await?;
        data.update_pull_request_review_comment
            .and_then(|p| p.pull_request_review_comment)
            .map(GqlComment::into_api)
            .ok_or_else(|| AppError::github("GitHub did not return the comment"))
    }

    /// Deletes the comment `comment_id` (a submitted one, or one in the pending review).
    pub async fn delete_comment(
        &self,
        host: &str,
        token: &str,
        comment_id: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .mutate(
                host,
                token,
                DELETE_COMMENT,
                json!({ "comment": comment_id }),
            )
            .await?;
        Ok(())
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

    /// An outdated thread: no current lines, only the original ones.
    fn outdated(
        id: &str,
        side: &str,
        start_side: &str,
        original_line: u32,
        original_start_line: Option<u32>,
    ) -> String {
        let c = comment(&format!("C{id}"), "SUBMITTED", "alice");
        let mut t: serde_json::Value = serde_json::from_str(&thread(
            id,
            "LINE",
            side,
            start_side,
            "null",
            "null",
            &[c],
            false,
        ))
        .unwrap();
        t["originalLine"] = json!(original_line);
        t["originalStartLine"] = json!(original_start_line);
        t["isOutdated"] = json!(true);
        t.to_string()
    }

    fn page(head: &str, threads: &[String], pending: &str, next: Option<&str>) -> ThreadsPr {
        let body = format!(
            r#"{{"data": {{"repository": {{"pullRequest": {{
                "headRefOid": "{head}", "locked": false,
                "reviews": {{"nodes": [{pending}]}},
                "reviewThreads": {{"pageInfo": {{"hasNextPage": {}, "endCursor": {}}}, "nodes": [{}]}}
            }}}}}}}}"#,
            next.is_some(),
            next.map_or("null".to_string(), |c| format!("\"{c}\"")),
            threads.join(",")
        );
        let resp: GqlResponse<ThreadsData> = serde_json::from_str(&body).unwrap();
        resp.data.unwrap().repository.unwrap().pull_request.unwrap()
    }

    fn parse(threads: &[String], pending: &str) -> ThreadsPr {
        page(HEAD, threads, pending, None)
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

    #[test]
    fn the_latest_review_and_the_pending_comment_count_are_read() {
        let body = format!(
            r#"{{"data": {{"repository": {{"pullRequest": {{
                "headRefOid": "{HEAD}", "locked": false,
                "reviews": {{"nodes": [{{"id": "R1", "body": "", "createdAt": "2026-10-13T00:00:00Z",
                    "url": "u", "viewerDidAuthor": true, "commit": null,
                    "commentCount": {{"totalCount": 2}}}}]}},
                "viewerLatestReview": {{"id": "R0", "state": "APPROVED", "url": "u0", "body": "LGTM"}},
                "reviewThreads": {{"pageInfo": {{"hasNextPage": false, "endCursor": null}}, "nodes": []}}
            }}}}}}}}"#
        );
        let resp: GqlResponse<ThreadsData> = serde_json::from_str(&body).unwrap();
        let pr = resp.data.unwrap().repository.unwrap().pull_request.unwrap();
        let latest = SubmittedReview {
            id: "R0".into(),
            state: "APPROVED".into(),
            url: "u0".into(),
            body: "LGTM".into(),
        };
        assert_eq!(pr.viewer_latest_review, Some(latest));
        assert_eq!(pending_review(pr.reviews).unwrap().comment_count, 2);
        // No review yet.
        let none = body.replace(
            r#"{"id": "R0", "state": "APPROVED", "url": "u0", "body": "LGTM"}"#,
            "null",
        );
        let resp: GqlResponse<ThreadsData> = serde_json::from_str(&none).unwrap();
        let pr = resp.data.unwrap().repository.unwrap().pull_request.unwrap();
        assert_eq!(pr.viewer_latest_review, None);
    }

    /// GraphQL rejects a query with an undefined or an unused fragment.
    #[test]
    fn queries_define_exactly_the_fragments_they_spread() {
        for q in [
            THREADS_QUERY,
            THREAD_COMMENTS_QUERY,
            REVIEW_STATE_QUERY,
            THREAD_QUERY,
            ADD_REVIEW,
            ADD_THREAD,
            SUBMIT_REVIEW,
            SUBMIT_NEW_REVIEW,
            DELETE_REVIEW,
            ADD_REPLY,
            RESOLVE_THREAD,
            UNRESOLVE_THREAD,
            UPDATE_COMMENT,
            DELETE_COMMENT,
        ] {
            for name in ["Comment", "Thread", "PendingReview"] {
                let spread =
                    q.contains(&format!("...{name} ")) || q.contains(&format!("...{name}\n"));
                let defined = q.contains(&format!("fragment {name} on"));
                assert_eq!(spread, defined, "{name} in {q}");
            }
        }
    }

    #[test]
    fn new_threads_become_graphql_variables() {
        let new = |target| NewThread {
            path: "src/a.rs".into(),
            body: "x".into(),
            target,
        };
        assert_eq!(
            new(ThreadTarget::File).line_fields(),
            json!({ "line": null, "side": null, "startLine": null, "startSide": null })
        );
        assert_eq!(
            new(ThreadTarget::Lines {
                side: DiffSide::Left,
                line: 9,
                start_line: None
            })
            .line_fields(),
            json!({ "line": 9, "side": "LEFT", "startLine": null, "startSide": null })
        );
        assert_eq!(
            new(ThreadTarget::Lines {
                side: DiffSide::Right,
                line: 9,
                start_line: Some(4)
            })
            .line_fields(),
            json!({ "line": 9, "side": "RIGHT", "startLine": 4, "startSide": "RIGHT" })
        );
    }

    #[test]
    fn written_reviews_and_threads_are_read_back() {
        let review: AddReviewData = serde_json::from_str(
            r#"{"addPullRequestReview": {"pullRequestReview": {"id": "PRR_9", "body": "",
                "createdAt": "2026-10-13T00:00:00Z", "url": "https://github.com/o/r/pull/1#pullrequestreview-9",
                "viewerDidAuthor": true, "commit": {"oid": "2222222222222222222222222222222222222222"},
                "comments": {"nodes": [{"id": "PRRC_1"}]}}}}"#,
        )
        .unwrap();
        let added = review
            .add_pull_request_review
            .and_then(|p| p.pull_request_review)
            .unwrap();
        assert_eq!(added.comments.into_vec()[0].id, "PRRC_1");
        assert_eq!(added.review.into_api().commit.as_deref(), Some(HEAD));

        // A submitted review, either way it was submitted.
        let want = SubmittedReview {
            id: "PRR_9".into(),
            state: "APPROVED".into(),
            url: "https://github.com/o/r/pull/1#pullrequestreview-9".into(),
            body: "ok".into(),
        };
        for field in ["submitPullRequestReview", "addPullRequestReview"] {
            let data: SubmitData = serde_json::from_str(&format!(
                r#"{{"{field}": {{"pullRequestReview": {{"id": "PRR_9", "state": "APPROVED",
                    "url": "https://github.com/o/r/pull/1#pullrequestreview-9", "body": "ok"}}}}}}"#
            ))
            .unwrap();
            assert_eq!(data.review(), Some(want.clone()), "{field}");
        }
        let none: SubmitData =
            serde_json::from_str(r#"{"submitPullRequestReview": null}"#).unwrap();
        assert_eq!(none.review(), None);

        // GitHub answers lines it does not take with a null thread and no error (spike S12).
        let none: AddThreadData =
            serde_json::from_str(r#"{"addPullRequestReviewThread": {"thread": null}}"#).unwrap();
        let line = ThreadTarget::Lines {
            side: DiffSide::Right,
            line: 1,
            start_line: None,
        };
        let err = added_thread(none, &line).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::Rejected, LINE_REJECTED)
        );
        let none: AddThreadData =
            serde_json::from_str(r#"{"addPullRequestReviewThread": null}"#).unwrap();
        assert_eq!(
            added_thread(none, &ThreadTarget::File).unwrap_err().code,
            ErrorCode::Rejected
        );
        let t = thread(
            "PRRT_9",
            "LINE",
            "RIGHT",
            "null",
            "3",
            "null",
            &[comment("C1", "PENDING", "me")],
            false,
        );
        let some: AddThreadData = serde_json::from_str(&format!(
            r#"{{"addPullRequestReviewThread": {{"thread": {t}}}}}"#
        ))
        .unwrap();
        let got = added_thread(some, &line).unwrap().unwrap();
        assert_eq!((got.id.as_str(), got.line), ("PRRT_9", Some(3)));
        // Created, but without its comment: not an error (it landed).
        let bare = thread("PRRT_10", "LINE", "RIGHT", "null", "3", "null", &[], false);
        let bare: AddThreadData = serde_json::from_str(&format!(
            r#"{{"addPullRequestReviewThread": {{"thread": {bare}}}}}"#
        ))
        .unwrap();
        assert!(added_thread(bare, &line).unwrap().is_none());

        // The single-comment review says it its own way (spike S12).
        let unresolved = line_rejection(AppError::rejected("Line could not be resolved"));
        assert_eq!(unresolved.message, LINE_REJECTED);
        let other = line_rejection(AppError::rejected("Can not approve your own pull request"));
        assert_eq!(other.message, "Can not approve your own pull request");

        // node(id:) of a thread, of something else, of nothing.
        let node: serde_json::Value = serde_json::from_str(&t.replacen(
            '{',
            r#"{"__typename": "PullRequestReviewThread", "#,
            1,
        ))
        .unwrap();
        assert_eq!(
            thread_node(Some(node)).unwrap().map(|t| t.id),
            Some("PRRT_9".into())
        );
        let other = json!({ "__typename": "Issue" });
        assert!(thread_node(Some(other)).unwrap().is_none());
        assert!(thread_node(None).unwrap().is_none());
        let broken = json!({ "__typename": "PullRequestReviewThread", "id": "PRRT_9" });
        assert_eq!(
            thread_node(Some(broken)).err().map(|e| e.code),
            Some(ErrorCode::GithubError)
        );
    }

    #[test]
    fn thread_writes_are_read_back() {
        let c = comment("PRRC_7", "PENDING", "me");
        let reply: AddReplyData = serde_json::from_str(&format!(
            r#"{{"addPullRequestReviewThreadReply": {{"comment": {c}}}}}"#
        ))
        .unwrap();
        let got = reply
            .add_pull_request_review_thread_reply
            .and_then(|p| p.comment)
            .map(GqlComment::into_api)
            .unwrap();
        assert_eq!((got.id.as_str(), got.state.as_str()), ("PRRC_7", "PENDING"));
        let none: AddReplyData =
            serde_json::from_str(r#"{"addPullRequestReviewThreadReply": {"comment": null}}"#)
                .unwrap();
        assert!(
            none.add_pull_request_review_thread_reply
                .and_then(|p| p.comment)
                .is_none()
        );

        let edited: UpdateCommentData = serde_json::from_str(&format!(
            r#"{{"updatePullRequestReviewComment": {{"pullRequestReviewComment": {c}}}}}"#
        ))
        .unwrap();
        assert!(
            edited
                .update_pull_request_review_comment
                .and_then(|p| p.pull_request_review_comment)
                .is_some()
        );

        // Both resolve mutations answer with the thread, under their own names.
        let t = thread("PRRT_3", "LINE", "RIGHT", "null", "3", "null", &[c], false);
        for name in ["resolveReviewThread", "unresolveReviewThread"] {
            let data: ResolveData =
                serde_json::from_str(&format!(r#"{{"{name}": {{"thread": {t}}}}}"#)).unwrap();
            assert_eq!(
                data.resolve_review_thread
                    .and_then(|p| p.thread)
                    .map(|t| t.id),
                Some("PRRT_3".into()),
                "{name}"
            );
        }
    }

    #[test]
    fn outdated_threads_keep_their_original_range() {
        let mut stale_start: serde_json::Value =
            serde_json::from_str(&outdated("O2", "RIGHT", "RIGHT", 20, Some(15))).unwrap();
        // A start without an end on the current commit is dropped.
        stale_start["startLine"] = json!(15);
        let t = normalize(parse(
            &[
                // Single line: the start echoes the line and has no side.
                outdated("O1", "RIGHT", "null", 40, Some(40)),
                stale_start.to_string(),
                outdated("O3", "RIGHT", "LEFT", 5, Some(9)),
                // Same side, start after the end: not a range.
                outdated("O4", "LEFT", "LEFT", 5, Some(9)),
            ],
            "",
        ));
        fn original(
            t: &ReviewThread,
        ) -> (Option<&str>, Option<&str>, Option<u32>, Option<u32>, bool) {
            (
                t.side.as_deref(),
                t.start_side.as_deref(),
                t.original_line,
                t.original_start_line,
                t.is_outdated,
            )
        }
        assert_eq!(original(&t[0]), (Some("RIGHT"), None, Some(40), None, true));
        assert_eq!(
            original(&t[1]),
            (Some("RIGHT"), Some("RIGHT"), Some(20), Some(15), true)
        );
        assert_eq!((t[1].line, t[1].start_line), (None, None));
        // Across sides the numbers are not comparable, so the start stays.
        assert_eq!(
            original(&t[2]),
            (Some("RIGHT"), Some("LEFT"), Some(5), Some(9), true)
        );
        assert_eq!(original(&t[3]), (Some("LEFT"), None, Some(5), None, true));
    }

    #[test]
    fn unknown_comment_states_count_as_submitted() {
        let t = normalize(parse(
            &[thread(
                "T1",
                "LINE",
                "RIGHT",
                "null",
                "3",
                "3",
                &[comment("C1", "SOMETHING_NEW", "a")],
                false,
            )],
            "",
        ));
        assert_eq!(t[0].comments[0].state, "SUBMITTED");
    }

    #[test]
    fn thread_pages_are_merged_once_and_must_share_a_head() {
        let c = |id: &str| comment(id, "SUBMITTED", "a");
        let t = |id: &str| thread(id, "LINE", "RIGHT", "null", "3", "3", &[c(id)], false);
        let mut pages = ThreadPages::default();
        assert!(matches!(
            pages.add(page(HEAD, &[t("T1"), t("T2")], "", Some("p2")), true),
            Ok(PageEnd::Next(c)) if c == "p2"
        ));
        // T2 shows up again after a thread before it was added.
        assert!(matches!(
            pages.add(page(HEAD, &[t("T2"), t("T3")], "", None), false),
            Ok(PageEnd::Done)
        ));
        let ids: Vec<&str> = pages.raw.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["T1", "T2", "T3"]);
        assert_eq!(pages.head, HEAD);

        let mut pages = ThreadPages::default();
        pages
            .add(page(HEAD, &[t("T1")], "", Some("p2")), true)
            .unwrap();
        let moved = "3333333333333333333333333333333333333333";
        assert!(matches!(
            pages.add(page(moved, &[t("T2")], "", None), false),
            Ok(PageEnd::HeadMoved)
        ));
        // A bad head on the first page is an error, not a retry.
        assert!(
            ThreadPages::default()
                .add(page("nope", &[], "", None), true)
                .is_err()
        );
    }
}
