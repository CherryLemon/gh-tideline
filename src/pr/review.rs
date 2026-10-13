//! Review threads and review writing for an open PR.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, ErrorCode};
use crate::github::{
    DiffSide, NewThread, PendingReview, ReviewComment, ReviewState, ReviewThread, ReviewThreads,
    ThreadTarget,
};
use crate::state::PrKey;

use super::{PrService, check_github_path};

/// GitHub's limit for a comment body, in characters.
const MAX_BODY_CHARS: usize = 65536;

/// `single`: post the comment now (GitHub's "Add single comment"); `review`: add it to the
/// viewer's pending review, starting one if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Single,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SubjectType {
    File,
    Line,
}

/// `POST /threads`: a new thread on a file, a line or a range of lines on one side.
/// Unknown fields are refused: a misspelt `startLine` must not turn a range into one line.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadRequest {
    /// The head the client shows; line numbers refer to it.
    pub head: String,
    pub path: String,
    pub body: String,
    pub mode: Mode,
    pub subject_type: SubjectType,
    pub side: Option<DiffSide>,
    pub line: Option<u32>,
    pub start_side: Option<DiffSide>,
    pub start_line: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedThread {
    /// `None` when the comment was posted but its thread could not be read back.
    pub thread: Option<ReviewThread>,
    pub pending_review: Option<PendingReview>,
    /// A single comment on a file was added but not submitted: it stays in the pending
    /// review, and this is GitHub's reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_error: Option<String>,
}

/// `DELETE /review`: the pending review the client shows, which is the one to discard.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscardRequest {
    pub review_id: String,
}

/// `POST /threads/{t}/replies`: a reply, posted now or added to the pending review.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplyRequest {
    /// The head the client shows: a reply that starts a review starts it there.
    pub head: String,
    pub body: String,
    pub mode: Mode,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Replied {
    /// `None` when GitHub took the reply but did not return it.
    pub comment: Option<ReviewComment>,
    pub pending_review: Option<PendingReview>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedComment {
    /// `None` once the thread has no comments left.
    pub thread: Option<ReviewThread>,
    pub pending_review: Option<PendingReview>,
}

/// `PUT /threads/{t}/resolved`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub resolved: bool,
}

/// `PATCH /threads/{t}/comments/{c}`: the comment's new text.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub body: String,
}

/// Rejects an empty body, or one longer than GitHub takes.
pub fn check_body(body: &str) -> Result<(), AppError> {
    if body.trim().is_empty() {
        return Err(AppError::bad_request("the comment is empty"));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(AppError::bad_request(format!(
            "the comment is longer than {MAX_BODY_CHARS} characters"
        )));
    }
    Ok(())
}

/// The thread a request asks for, if it is well formed: lines for a LINE thread (a range
/// starts before its end, on the same side), none for a FILE thread.
fn new_thread(req: &ThreadRequest) -> Result<NewThread, AppError> {
    check_body(&req.body)?;
    let target = match req.subject_type {
        SubjectType::File => {
            if req.side.is_some()
                || req.line.is_some()
                || req.start_side.is_some()
                || req.start_line.is_some()
            {
                return Err(AppError::bad_request(
                    "a comment on a file has no side or lines",
                ));
            }
            ThreadTarget::File
        }
        SubjectType::Line => {
            let (Some(side), Some(line)) = (req.side, req.line) else {
                return Err(AppError::bad_request(
                    "a comment on lines needs a side and a line",
                ));
            };
            let valid = |n: u32| n >= 1 && n <= i32::MAX as u32;
            if !valid(line) {
                return Err(AppError::bad_request(format!("invalid line {line}")));
            }
            match (req.start_line, req.start_side) {
                (None, None) => {}
                (Some(start), start_side) if valid(start) && start < line => {
                    if start_side.is_some_and(|s| s != side) {
                        return Err(AppError::bad_request(
                            "a range of lines must be on one side",
                        ));
                    }
                }
                (Some(start), _) => {
                    return Err(AppError::bad_request(format!(
                        "invalid range of lines {start} to {line}"
                    )));
                }
                (None, Some(_)) => {
                    return Err(AppError::bad_request("a start side needs a start line"));
                }
            }
            ThreadTarget::Lines {
                side,
                line,
                start_line: req.start_line,
            }
        }
    };
    Ok(NewThread {
        path: req.path.clone(),
        body: req.body.clone(),
        target,
    })
}

/// Rejects writes the viewer's pending review stands in the way of (409), or that would land
/// on another head than the one the client shows (409 `stale_head`). Line numbers refer to
/// the client's head, and GitHub anchors new threads to its current head (spike S13d); a new
/// review starts on the head as well. A file comment or a reply added to an existing review
/// has neither. `listed_head`: the head of the file list a path was checked against.
fn check_write(
    mode: Mode,
    on_lines: bool,
    state: &ReviewState,
    client_head: &str,
    listed_head: Option<&str>,
) -> Result<(), AppError> {
    if mode == Mode::Single && state.pending.is_some() {
        return Err(AppError::pending_review_exists(
            "you have a pending review: add the comment to it, or submit or discard it first",
        ));
    }
    let heads_differ =
        state.head != client_head || listed_head.is_some_and(|listed| listed != client_head);
    if (on_lines || state.pending.is_none()) && heads_differ {
        return Err(AppError::stale_head(
            "the pull request has new commits; refresh before commenting",
        ));
    }
    Ok(())
}

/// The review to delete for a discard of `shown`: none when there is no pending review any
/// more (already gone); 409 when GitHub has another one (the client never saw its comments).
fn review_to_discard<'a>(
    pending: Option<&'a PendingReview>,
    shown: &str,
) -> Result<Option<&'a str>, AppError> {
    match pending {
        None => Ok(None),
        Some(review) if review.id == shown => Ok(Some(&review.id)),
        Some(_) => Err(AppError::review_changed(
            "your pending review on GitHub is not the one shown; refresh to see it",
        )),
    }
}

/// A pending review that went away between reading it and writing to it (submitted or
/// discarded meanwhile, or the read lagged behind a discard).
fn gone_review(e: AppError) -> AppError {
    if e.code == ErrorCode::NotFound {
        AppError::review_changed(
            "your pending review is no longer on GitHub; refresh and try again",
        )
    } else {
        e
    }
}

/// A thread or comment deleted meanwhile (on github.com, or in another tab), said plainly.
fn gone(e: AppError, what: &str) -> AppError {
    if e.code == ErrorCode::NotFound {
        eprintln!("gh-tideline: {what} was not found: {e}");
        AppError::not_found(format!(
            "{what} is no longer on GitHub; refresh to see the rest"
        ))
    } else {
        e
    }
}

/// A failed reply into the pending review `review_id`: GitHub names the node it can't find.
fn reply_error(e: AppError, review_id: &str) -> AppError {
    if e.code == ErrorCode::NotFound && e.message.contains(review_id) {
        gone_review(e)
    } else {
        gone(e, "this thread")
    }
}

/// Starting a review failed because the viewer has one now (started on github.com since it
/// was read): like a pending review that changed.
fn start_error(e: AppError) -> AppError {
    if e.code == ErrorCode::Rejected && e.message.contains("one pending review") {
        AppError::review_changed(
            "you started a pending review elsewhere meanwhile; refresh to see it",
        )
    } else {
        e
    }
}

/// A delete of `comment_id` that found it gone already counts as done (GitHub names the node
/// it can't find); any other failure stands.
fn deleted_already(result: Result<(), AppError>, comment_id: &str) -> Result<(), AppError> {
    match result {
        Err(e) if e.code == ErrorCode::NotFound && e.message.contains(comment_id) => Ok(()),
        other => other,
    }
}

/// A read after a delete that landed failed: the delete isn't what failed.
fn read_back_error(mut e: AppError) -> AppError {
    e.message = format!(
        "the comment was deleted, but reading its thread back failed: {}",
        e.message
    );
    e
}

/// A thread read back after deleting `comment_id`, in case the read still has it; `None` once
/// no comment is left (GitHub drops the thread).
fn without_comment(thread: Option<ReviewThread>, comment_id: &str) -> Option<ReviewThread> {
    let mut thread = thread?;
    thread.comments.retain(|c| c.id != comment_id);
    (!thread.comments.is_empty()).then_some(thread)
}

/// Says so when the cleanup after a failed write failed too: the empty review it left would
/// otherwise block single comments with no hint why.
fn with_cleanup(mut e: AppError, cleaned: bool) -> AppError {
    if !cleaned {
        e.message
            .push_str(" (an empty pending review was left on GitHub: discard it)");
    }
    e
}

impl PrService {
    /// The PR's review threads and the viewer's pending review, read fresh (never cached:
    /// other reviewers and github.com change them at any time).
    pub async fn threads(&self, key: &PrKey) -> Result<ReviewThreads, AppError> {
        let token = self.tokens.get(&key.host).await?;
        self.github
            .review_threads(&key.host, &token, &key.owner, &key.repo, key.number)
            .await
    }

    /// Review writes to one PR run one at a time, from reading the viewer's pending review to
    /// the last write: two at once could add to a review the other is about to submit or
    /// delete, or both start one.
    async fn review_write(&self, key: &PrKey) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.review_writes.lock().expect("review locks poisoned");
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(locks.entry(key.clone()).or_default())
        };
        lock.lock_owned().await
    }

    /// Starts a thread, as a single comment or in the viewer's pending review (see [`Mode`]).
    /// Nothing is retried: a create that failed midway may have landed.
    pub async fn create_thread(
        self: &Arc<Self>,
        key: &PrKey,
        req: &ThreadRequest,
    ) -> Result<CreatedThread, AppError> {
        let thread = new_thread(req)?;
        // First: without a snapshot of the client's head this loads the PR afresh (a fetch),
        // which must not come between reading the head and writing.
        let data = self.snapshot_for_write(key, &req.head).await?;
        check_github_path(&data, &thread.path)?;
        let token = self.tokens.get(&key.host).await?;
        let _writing = self.review_write(key).await;
        let state = self
            .github
            .review_state(&key.host, &token, &key.owner, &key.repo, key.number)
            .await?;
        let on_lines = thread.target != ThreadTarget::File;
        check_write(
            req.mode,
            on_lines,
            &state,
            &req.head,
            Some(&data.summary.head.sha),
        )?;
        match (req.mode, &thread.target) {
            (Mode::Single, ThreadTarget::Lines { .. }) => {
                self.comment_on_lines(key, &token, &state, &thread).await
            }
            (Mode::Single, ThreadTarget::File) => {
                self.comment_on_file(&key.host, &token, &state, &thread)
                    .await
            }
            (Mode::Review, _) => self.add_to_review(&key.host, &token, state, &thread).await,
        }
    }

    /// One call, so GitHub refusing the lines leaves nothing behind. GitHub returns the review,
    /// not the thread: it comes from a fresh read, and is `None` if that fails (the comment is
    /// posted all the same).
    async fn comment_on_lines(
        &self,
        key: &PrKey,
        token: &str,
        state: &ReviewState,
        thread: &NewThread,
    ) -> Result<CreatedThread, AppError> {
        let comment = self
            .github
            .comment_on_lines(&key.host, token, &state.pr_id, &state.head, thread)
            .await?;
        let read = self
            .github
            .review_threads(&key.host, token, &key.owner, &key.repo, key.number)
            .await;
        let thread = match (read, comment) {
            (Ok(read), Some(id)) => read
                .threads
                .into_iter()
                .find(|t| t.comments.iter().any(|c| c.id == id)),
            _ => None,
        };
        Ok(CreatedThread {
            thread,
            pending_review: None,
            submit_error: None,
        })
    }

    /// A single comment on a file needs a review of its own: start one, add the thread and
    /// submit it. If the thread fails, the empty review goes; if the submit fails, the comment
    /// stays pending and the client is told why.
    async fn comment_on_file(
        &self,
        host: &str,
        token: &str,
        state: &ReviewState,
        thread: &NewThread,
    ) -> Result<CreatedThread, AppError> {
        let review = self
            .github
            .start_review(host, token, &state.pr_id, &state.head)
            .await
            .map_err(start_error)?;
        let added = match self
            .github
            .add_thread(host, token, &review.id, thread)
            .await
        {
            Ok(added) => added,
            Err(e) => {
                let cleaned = self.delete_review_quietly(host, token, &review.id).await;
                return Err(with_cleanup(e, cleaned));
            }
        };
        if let Err(e) = self
            .github
            .submit_review(host, token, &review.id, "COMMENT", None)
            .await
        {
            return Ok(CreatedThread {
                thread: added,
                pending_review: Some(review),
                submit_error: Some(e.message),
            });
        }
        // Read back: the comments are no longer pending.
        let thread = match added {
            Some(added) => self
                .github
                .review_thread(host, token, &added.id)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        Ok(CreatedThread {
            thread,
            pending_review: None,
            submit_error: None,
        })
    }

    /// Adds the thread to the pending review, starting one on the head if there is none (and
    /// deleting it again if GitHub refuses the thread).
    async fn add_to_review(
        &self,
        host: &str,
        token: &str,
        state: ReviewState,
        thread: &NewThread,
    ) -> Result<CreatedThread, AppError> {
        let (review, started) = match state.pending {
            Some(review) => (review, false),
            None => (
                self.github
                    .start_review(host, token, &state.pr_id, &state.head)
                    .await
                    .map_err(start_error)?,
                true,
            ),
        };
        match self
            .github
            .add_thread(host, token, &review.id, thread)
            .await
        {
            Ok(added) => Ok(CreatedThread {
                thread: added,
                pending_review: Some(review),
                submit_error: None,
            }),
            Err(e) if started => {
                let cleaned = self.delete_review_quietly(host, token, &review.id).await;
                Err(with_cleanup(e, cleaned))
            }
            Err(e) => Err(gone_review(e)),
        }
    }

    /// Deletes the viewer's pending review `shown` and all its comments. Done already (or
    /// never there) counts as done; another pending review is not touched (409).
    pub async fn discard_review(&self, key: &PrKey, shown: &str) -> Result<(), AppError> {
        let token = self.tokens.get(&key.host).await?;
        let _writing = self.review_write(key).await;
        let state = self
            .github
            .review_state(&key.host, &token, &key.owner, &key.repo, key.number)
            .await?;
        let Some(id) = review_to_discard(state.pending.as_ref(), shown)? else {
            return Ok(());
        };
        match self.github.delete_review(&key.host, &token, id).await {
            // A read right after a discard can still list the review (spike S13).
            Err(e) if e.code == ErrorCode::NotFound => Ok(()),
            done => done,
        }
    }

    /// Replies to `thread_id`, as a single comment or in the viewer's pending review (see
    /// [`Mode`]). Nothing is retried: a reply that failed midway may have landed.
    pub async fn reply(
        &self,
        key: &PrKey,
        thread_id: &str,
        req: &ReplyRequest,
    ) -> Result<Replied, AppError> {
        check_body(&req.body)?;
        let host = &key.host;
        let token = self.tokens.get(host).await?;
        let _writing = self.review_write(key).await;
        let state = self
            .github
            .review_state(host, &token, &key.owner, &key.repo, key.number)
            .await?;
        check_write(req.mode, false, &state, &req.head, None)?;
        if req.mode == Mode::Single {
            let comment = self
                .github
                .add_reply(host, &token, thread_id, None, &req.body)
                .await
                .map_err(|e| gone(e, "this thread"))?;
            // A reply sent without a review goes into the viewer's pending review if there is
            // one (spike S13): one started since the read above. Then it is that review's.
            let pending_review = match &comment {
                Some(c) if c.state == "PENDING" => self
                    .github
                    .review_state(host, &token, &key.owner, &key.repo, key.number)
                    .await
                    .ok()
                    .and_then(|state| state.pending),
                _ => None,
            };
            return Ok(Replied {
                comment,
                pending_review,
            });
        }
        let (review, started) = match state.pending {
            Some(review) => (review, false),
            None => (
                self.github
                    .start_review(host, &token, &state.pr_id, &state.head)
                    .await
                    .map_err(start_error)?,
                true,
            ),
        };
        match self
            .github
            .add_reply(host, &token, thread_id, Some(&review.id), &req.body)
            .await
        {
            // GitHub answers a thread it won't take with nothing (spike S12); a reply too, for
            // all we know. The review started for it would be left empty.
            Ok(None) if started => {
                let cleaned = self.delete_review_quietly(host, &token, &review.id).await;
                Err(with_cleanup(
                    AppError::rejected("GitHub did not take the reply"),
                    cleaned,
                ))
            }
            Ok(comment) => Ok(Replied {
                comment,
                pending_review: Some(review),
            }),
            Err(e) if started => {
                let cleaned = self.delete_review_quietly(host, &token, &review.id).await;
                Err(with_cleanup(gone(e, "this thread"), cleaned))
            }
            Err(e) => Err(reply_error(e, &review.id)),
        }
    }

    /// Resolves or unresolves a thread; it comes back as GitHub has it now.
    pub async fn set_resolved(
        &self,
        key: &PrKey,
        thread_id: &str,
        resolved: bool,
    ) -> Result<ReviewThread, AppError> {
        let token = self.tokens.get(&key.host).await?;
        self.github
            .set_resolved(&key.host, &token, thread_id, resolved)
            .await
            .map_err(|e| gone(e, "this thread"))
    }

    /// Replaces the text of one of the viewer's comments.
    pub async fn edit_comment(
        &self,
        key: &PrKey,
        comment_id: &str,
        body: &str,
    ) -> Result<ReviewComment, AppError> {
        check_body(body)?;
        let token = self.tokens.get(&key.host).await?;
        self.github
            .update_comment(&key.host, &token, comment_id, body)
            .await
            .map_err(|e| gone(e, "this comment"))
    }

    /// Deletes a comment, then reads its thread and the pending review again: the thread is
    /// `None` once it has no comments left, and GitHub drops a pending review with its last
    /// comment. A comment that is gone already counts as deleted, so a delete whose answer was
    /// lost can be sent again.
    pub async fn delete_comment(
        &self,
        key: &PrKey,
        thread_id: &str,
        comment_id: &str,
    ) -> Result<DeletedComment, AppError> {
        let host = &key.host;
        let token = self.tokens.get(host).await?;
        // A create that read the pending review must not find it dropped before it writes.
        let _writing = self.review_write(key).await;
        deleted_already(
            self.github.delete_comment(host, &token, comment_id).await,
            comment_id,
        )?;
        let (thread, state) = tokio::join!(
            self.github.review_thread(host, &token, thread_id),
            self.github
                .review_state(host, &token, &key.owner, &key.repo, key.number),
        );
        Ok(DeletedComment {
            thread: without_comment(thread.map_err(read_back_error)?, comment_id),
            pending_review: state.map_err(read_back_error)?.pending,
        })
    }

    /// Cleanup after a failed write; the write's own error is the one to report. Whether the
    /// review is gone.
    async fn delete_review_quietly(&self, host: &str, token: &str, review_id: &str) -> bool {
        match self.github.delete_review(host, token, review_id).await {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "gh-tideline: deleting the review started for a failed comment failed: {e}"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn req(subject: SubjectType) -> ThreadRequest {
        ThreadRequest {
            head: HEAD.into(),
            path: "src/a.ts".into(),
            body: "Looks off".into(),
            mode: Mode::Review,
            subject_type: subject,
            side: None,
            line: None,
            start_side: None,
            start_line: None,
        }
    }

    fn lines(
        side: Option<DiffSide>,
        line: Option<u32>,
        start_side: Option<DiffSide>,
        start_line: Option<u32>,
    ) -> ThreadRequest {
        ThreadRequest {
            side,
            line,
            start_side,
            start_line,
            ..req(SubjectType::Line)
        }
    }

    fn code<T>(r: Result<T, AppError>) -> Result<T, ErrorCode> {
        r.map_err(|e| e.code)
    }

    use DiffSide::{Left, Right};

    #[test]
    fn requests_parse_from_the_client_shape() {
        let r: ThreadRequest = serde_json::from_str(&format!(
            r#"{{"head": "{HEAD}", "path": "a", "body": "x", "mode": "single",
                "subjectType": "LINE", "side": "LEFT", "line": 4, "startLine": 2}}"#
        ))
        .unwrap();
        assert_eq!(
            (r.mode, r.subject_type, r.side, r.start_side),
            (Mode::Single, SubjectType::Line, Some(Left), None)
        );
        let file: ThreadRequest = serde_json::from_str(&format!(
            r#"{{"head": "{HEAD}", "path": "a", "body": "x", "mode": "review", "subjectType": "FILE"}}"#
        ))
        .unwrap();
        assert_eq!(file.line, None);
        for bad in [
            r#""mode": "later", "subjectType": "FILE""#,
            r#""mode": "review", "subjectType": "file""#,
            r#""mode": "review", "subjectType": "LINE", "side": "BOTH", "line": 1"#,
            r#""mode": "review", "subjectType": "LINE", "side": "LEFT", "line": -1"#,
        ] {
            let json = format!(r#"{{"head": "{HEAD}", "path": "a", "body": "x", {bad}}}"#);
            assert!(
                serde_json::from_str::<ThreadRequest>(&json).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn line_threads_need_a_side_a_line_and_a_range_on_one_side() {
        let ok = |r: ThreadRequest| code(new_thread(&r)).map(|t| t.target);
        assert_eq!(
            ok(lines(Some(Right), Some(7), None, None)),
            Ok(ThreadTarget::Lines {
                side: Right,
                line: 7,
                start_line: None
            })
        );
        assert_eq!(
            ok(lines(Some(Left), Some(7), Some(Left), Some(3))),
            Ok(ThreadTarget::Lines {
                side: Left,
                line: 7,
                start_line: Some(3)
            })
        );
        // The start side defaults to the side.
        assert!(ok(lines(Some(Left), Some(7), None, Some(3))).is_ok());
        for bad in [
            lines(None, Some(7), None, None),
            lines(Some(Right), None, None, None),
            lines(Some(Right), Some(0), None, None),
            lines(Some(Right), Some(u32::MAX), None, None),
            lines(Some(Right), Some(7), None, Some(7)),
            lines(Some(Right), Some(7), None, Some(9)),
            lines(Some(Right), Some(7), None, Some(0)),
            lines(Some(Right), Some(7), Some(Left), Some(3)),
            lines(Some(Right), Some(7), Some(Right), None),
        ] {
            assert_eq!(ok(bad.clone()), Err(ErrorCode::BadRequest), "{bad:?}");
        }
    }

    #[test]
    fn file_threads_have_no_lines() {
        assert_eq!(
            code(new_thread(&req(SubjectType::File))).map(|t| t.target),
            Ok(ThreadTarget::File)
        );
        let with_line = ThreadRequest {
            line: Some(1),
            ..req(SubjectType::File)
        };
        assert_eq!(
            code(new_thread(&with_line)).map(|t| t.target),
            Err(ErrorCode::BadRequest)
        );
        let with_side = ThreadRequest {
            side: Some(Right),
            ..req(SubjectType::File)
        };
        assert!(new_thread(&with_side).is_err());
    }

    #[test]
    fn bodies_must_have_text_and_fit_on_github() {
        assert!(check_body("x").is_ok());
        assert!(check_body(&"é".repeat(MAX_BODY_CHARS)).is_ok());
        for bad in [
            String::new(),
            " \n\t".into(),
            "é".repeat(MAX_BODY_CHARS + 1),
        ] {
            assert_eq!(code(check_body(&bad)), Err(ErrorCode::BadRequest));
        }
    }

    fn state(head: &str, pending: bool) -> ReviewState {
        ReviewState {
            pr_id: "PR_1".into(),
            head: head.into(),
            pending: pending.then(|| PendingReview {
                id: "PRR_1".into(),
                body: String::new(),
                commit: Some(OTHER.into()),
                created_at: "2026-10-13T00:00:00Z".into(),
                url: "https://github.com/o/r/pull/1".into(),
            }),
        }
    }

    #[test]
    fn creates_check_the_pending_review_and_the_head() {
        let line = new_thread(&lines(Some(Right), Some(3), None, None)).unwrap();
        let file = new_thread(&req(SubjectType::File)).unwrap();
        let check = |mode, t: &NewThread, s: &ReviewState| {
            code(check_write(
                mode,
                t.target != ThreadTarget::File,
                s,
                HEAD,
                Some(HEAD),
            ))
        };
        // Single comments only without a pending review.
        assert_eq!(check(Mode::Single, &line, &state(HEAD, false)), Ok(()));
        assert_eq!(
            check(Mode::Single, &line, &state(HEAD, true)),
            Err(ErrorCode::PendingReviewExists)
        );
        assert_eq!(
            check(Mode::Single, &file, &state(HEAD, true)),
            Err(ErrorCode::PendingReviewExists)
        );
        assert_eq!(check(Mode::Review, &line, &state(HEAD, true)), Ok(()));
        // Lines refer to the client's head.
        for mode in [Mode::Single, Mode::Review] {
            for pending in [false, true] {
                if mode == Mode::Single && pending {
                    continue;
                }
                assert_eq!(
                    check(mode, &line, &state(OTHER, pending)),
                    Err(ErrorCode::StaleHead)
                );
            }
        }
        // A file comment needs the client's head only to start a review.
        assert_eq!(
            check(Mode::Review, &file, &state(OTHER, false)),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(
            check(Mode::Single, &file, &state(OTHER, false)),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(check(Mode::Review, &file, &state(OTHER, true)), Ok(()));
        // The path was checked against another head's files (a push came in while loading).
        let listed_other = |mode, t: &NewThread, pending| {
            let on_lines = t.target != ThreadTarget::File;
            code(check_write(
                mode,
                on_lines,
                &state(HEAD, pending),
                HEAD,
                Some(OTHER),
            ))
        };
        assert_eq!(
            listed_other(Mode::Review, &line, true),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(
            listed_other(Mode::Single, &file, false),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(listed_other(Mode::Review, &file, true), Ok(()));
    }

    #[test]
    fn replies_check_the_pending_review_and_the_head() {
        let check = |mode, s: &ReviewState| code(check_write(mode, false, s, HEAD, None));
        assert_eq!(check(Mode::Single, &state(HEAD, false)), Ok(()));
        assert_eq!(
            check(Mode::Single, &state(HEAD, true)),
            Err(ErrorCode::PendingReviewExists)
        );
        assert_eq!(check(Mode::Review, &state(HEAD, true)), Ok(()));
        // Starting a review (a single reply is one too) needs the client's head.
        assert_eq!(
            check(Mode::Single, &state(OTHER, false)),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(
            check(Mode::Review, &state(OTHER, false)),
            Err(ErrorCode::StaleHead)
        );
        // Adding to the pending review doesn't.
        assert_eq!(check(Mode::Review, &state(OTHER, true)), Ok(()));
    }

    #[test]
    fn writes_on_deleted_nodes_say_what_went() {
        let missing = |id: &str| {
            AppError::not_found(format!(
                "Could not resolve to a node with the global id of '{id}'"
            ))
        };
        let thread = gone(missing("PRRT_1"), "this thread");
        assert_eq!(thread.code, ErrorCode::NotFound);
        assert!(
            thread
                .message
                .starts_with("this thread is no longer on GitHub")
        );
        assert_eq!(
            gone(AppError::rejected("no"), "this thread"),
            AppError::rejected("no")
        );
        // A reply names whichever of the thread and the pending review went.
        assert_eq!(
            reply_error(missing("PRR_1"), "PRR_1").code,
            ErrorCode::ReviewChanged
        );
        let thread = reply_error(missing("PRRT_1"), "PRR_1");
        assert_eq!(thread.code, ErrorCode::NotFound);
        assert!(thread.message.starts_with("this thread"));
        assert_eq!(
            reply_error(AppError::rejected("locked"), "PRR_1"),
            AppError::rejected("locked")
        );
    }

    fn mk_comment(id: &str) -> ReviewComment {
        ReviewComment {
            id: id.into(),
            author: None,
            body: "b".into(),
            body_html: "<p>b</p>".into(),
            created_at: "2026-10-13T00:00:00Z".into(),
            last_edited_at: None,
            url: "https://github.com/o/r/pull/1#discussion_r1".into(),
            state: "PENDING".into(),
            review_id: Some("PRR_1".into()),
            reply_to: None,
            is_minimized: false,
            minimized_reason: None,
            viewer_can_update: true,
            viewer_can_delete: true,
            viewer_did_author: true,
        }
    }

    fn mk_thread(comments: &[&str]) -> ReviewThread {
        ReviewThread {
            id: "PRRT_1".into(),
            path: "a.rs".into(),
            subject_type: "FILE".into(),
            side: None,
            start_side: None,
            line: None,
            start_line: None,
            original_line: None,
            original_start_line: None,
            diff_hunk: String::new(),
            is_resolved: false,
            is_outdated: false,
            is_collapsed: false,
            resolved_by: None,
            viewer_can_reply: true,
            viewer_can_resolve: false,
            viewer_can_unresolve: false,
            comments: comments.iter().map(|id| mk_comment(id)).collect(),
        }
    }

    #[test]
    fn deletes_count_only_their_own_comment_as_gone() {
        let missing = |id: &str| {
            AppError::not_found(format!(
                "Could not resolve to a node with the global id of '{id}'"
            ))
        };
        assert_eq!(deleted_already(Ok(()), "PRRC_1"), Ok(()));
        assert_eq!(deleted_already(Err(missing("PRRC_1")), "PRRC_1"), Ok(()));
        // Something else not found (a wrong host's endpoint, another node) is a failure.
        assert_eq!(
            code(deleted_already(Err(missing("PRRT_1")), "PRRC_1")),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            deleted_already(Err(AppError::rejected("no")), "PRRC_1"),
            Err(AppError::rejected("no"))
        );
        assert_eq!(
            code(deleted_already(Err(AppError::github("down")), "PRRC_1")),
            Err(ErrorCode::GithubError)
        );
        let late = read_back_error(AppError::github("timed out"));
        assert_eq!(late.code, ErrorCode::GithubError);
        assert!(late.message.starts_with("the comment was deleted"));
    }

    #[test]
    fn a_thread_read_back_leaves_the_deleted_comment_out() {
        let kept = without_comment(Some(mk_thread(&["PRRC_1", "PRRC_2"])), "PRRC_1").unwrap();
        assert_eq!(kept.comments.len(), 1);
        assert_eq!(kept.comments[0].id, "PRRC_2");
        assert!(without_comment(Some(mk_thread(&["PRRC_1"])), "PRRC_1").is_none());
        assert!(without_comment(None, "PRRC_1").is_none());
    }

    #[test]
    fn a_review_started_elsewhere_meanwhile_is_a_changed_review() {
        let raced = start_error(AppError::rejected(
            "User can only have one pending review per pull request",
        ));
        assert_eq!(raced.code, ErrorCode::ReviewChanged);
        assert_eq!(
            start_error(AppError::rejected("Pull request is locked")),
            AppError::rejected("Pull request is locked")
        );
    }

    #[test]
    fn thread_write_answers_keep_their_nulls() {
        let replied = serde_json::to_value(Replied {
            comment: None,
            pending_review: None,
        })
        .unwrap();
        assert_eq!(
            replied,
            serde_json::json!({ "comment": null, "pendingReview": null })
        );
        let deleted = serde_json::to_value(DeletedComment {
            thread: None,
            pending_review: None,
        })
        .unwrap();
        assert_eq!(
            deleted,
            serde_json::json!({ "thread": null, "pendingReview": null })
        );
    }

    #[test]
    fn thread_write_requests_parse() {
        let reply: ReplyRequest = serde_json::from_str(&format!(
            r#"{{"head": "{HEAD}", "body": "yes", "mode": "review"}}"#
        ))
        .unwrap();
        assert_eq!((reply.mode, reply.body.as_str()), (Mode::Review, "yes"));
        assert!(serde_json::from_str::<ReplyRequest>(r#"{"head": "x", "body": "y"}"#).is_err());
        let resolve: ResolveRequest = serde_json::from_str(r#"{"resolved": false}"#).unwrap();
        assert!(!resolve.resolved);
        assert!(serde_json::from_str::<ResolveRequest>(r#"{"resolve": true}"#).is_err());
        let edit: EditRequest = serde_json::from_str(r#"{"body": "fixed"}"#).unwrap();
        assert_eq!(edit.body, "fixed");
        assert!(serde_json::from_str::<EditRequest>(r#"{"body": "a", "id": "b"}"#).is_err());
    }

    #[test]
    fn requests_with_unknown_fields_are_refused() {
        let typo = r#"{"head": "1111111111111111111111111111111111111111", "path": "a",
            "body": "b", "mode": "review", "subjectType": "LINE", "side": "RIGHT", "line": 9,
            "start_line": 3}"#;
        assert!(serde_json::from_str::<ThreadRequest>(typo).is_err());
        let discard: DiscardRequest = serde_json::from_str(r#"{"reviewId": "PRR_1"}"#).unwrap();
        assert_eq!(discard.review_id, "PRR_1");
        assert!(serde_json::from_str::<DiscardRequest>(r#"{"id": "PRR_1"}"#).is_err());
    }

    #[test]
    fn discards_touch_only_the_review_shown() {
        let pending = state(HEAD, true).pending;
        assert_eq!(
            review_to_discard(pending.as_ref(), "PRR_1"),
            Ok(Some("PRR_1"))
        );
        // Gone already: nothing to do.
        assert_eq!(review_to_discard(None, "PRR_1"), Ok(None));
        // Another review (started on github.com after the one shown was submitted).
        assert_eq!(
            code(review_to_discard(pending.as_ref(), "PRR_0")),
            Err(ErrorCode::ReviewChanged)
        );
    }

    #[test]
    fn failed_writes_say_what_they_left() {
        assert_eq!(
            gone_review(AppError::not_found("Could not resolve to a node")).code,
            ErrorCode::ReviewChanged
        );
        assert_eq!(
            gone_review(AppError::rejected("no")).code,
            ErrorCode::Rejected
        );
        let left = with_cleanup(AppError::rejected("no"), false);
        assert_eq!(left.code, ErrorCode::Rejected);
        assert!(left.message.starts_with("no (an empty pending review"));
        assert_eq!(with_cleanup(AppError::rejected("no"), true).message, "no");
    }
}
