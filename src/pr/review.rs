//! Review threads and review writing for an open PR.

use crate::error::AppError;
use crate::github::ReviewThreads;
use crate::state::PrKey;

use super::PrService;

impl PrService {
    /// The PR's review threads and the viewer's pending review, read fresh (never cached:
    /// other reviewers and github.com change them at any time).
    pub async fn threads(&self, key: &PrKey) -> Result<ReviewThreads, AppError> {
        let token = self.tokens.get(&key.host).await?;
        self.github
            .review_threads(&key.host, &token, &key.owner, &key.repo, key.number)
            .await
    }
}
