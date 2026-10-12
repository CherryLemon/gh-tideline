//! Parsing of the CLI `TARGET` argument and of git remote URLs.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    /// SPA route for this PR.
    pub fn web_path(&self) -> String {
        format!(
            "/{}/{}/{}/pull/{}",
            self.host, self.owner, self.repo, self.number
        )
    }
}

/// Owner and repo names must be safe as path components of the cache directory.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !s.starts_with('.')
}

fn parse_number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().filter(|&n| n > 0)
}

/// Parses a `TARGET`:
/// - `https://<host>/o/r/pull/123[/files...]` (any host, so GHE works too)
/// - `o/r#123` (github.com)
/// - `123` or `#123`, resolved against `origin_url()` (the cwd's `origin` remote)
pub fn parse_target(
    input: &str,
    origin_url: impl FnOnce() -> Result<String, String>,
) -> Result<PrRef, String> {
    let input = input.trim();
    let bad = || {
        format!(
            "cannot parse {input:?}; expected a PR URL, owner/repo#123, or a PR number inside a clone"
        )
    };

    if let Some(n) = parse_number(input.strip_prefix('#').unwrap_or(input)) {
        let url = origin_url()?;
        let (host, owner, repo) = parse_remote_url(&url)
            .ok_or_else(|| format!("cannot infer a GitHub repo from origin remote {url:?}"))?;
        return Ok(PrRef {
            host,
            owner,
            repo,
            number: n,
        });
    }

    if let Some(rest) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        let rest = rest.split(['?', '#']).next().unwrap_or_default();
        let mut segs = rest.split('/');
        let host = segs.next().unwrap_or_default().to_ascii_lowercase();
        let owner = segs.next().unwrap_or_default();
        let repo = segs.next().unwrap_or_default();
        let kind = segs.next().unwrap_or_default();
        let number = segs.next().and_then(parse_number);
        return match number {
            Some(number)
                if valid_host(&host)
                    && valid_name(owner)
                    && valid_name(repo)
                    && (kind == "pull" || kind == "pulls") =>
            {
                Ok(PrRef {
                    host,
                    owner: owner.into(),
                    repo: repo.into(),
                    number,
                })
            }
            _ => Err(bad()),
        };
    }

    if let Some((slug, num)) = input.split_once('#')
        && let Some((owner, repo)) = slug.split_once('/')
        && let Some(number) = parse_number(num)
        && valid_name(owner)
        && valid_name(repo)
    {
        return Ok(PrRef {
            host: "github.com".into(),
            owner: owner.into(),
            repo: repo.into(),
            number,
        });
    }

    Err(bad())
}

/// Extracts `(host, owner, repo)` from an ssh, scp-like or https git remote URL.
pub fn parse_remote_url(url: &str) -> Option<(String, String, String)> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(
            scheme,
            "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host_port.split(':').next()?;
        (host.to_string(), path)
    } else {
        // scp-like syntax: [user@]host:owner/repo.git
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (host.to_string(), path)
    };
    let mut host = host.to_ascii_lowercase();
    if host == "ssh.github.com" {
        host = "github.com".into();
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    (valid_host(&host) && valid_name(owner) && valid_name(repo))
        .then(|| (host, owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(host: &str, owner: &str, repo: &str, number: u64) -> PrRef {
        PrRef {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
            number,
        }
    }

    fn no_origin() -> Result<String, String> {
        panic!("origin must not be consulted")
    }

    #[test]
    fn github_url_forms() {
        let want = pr("github.com", "cli", "cli", 14507);
        for input in [
            "https://github.com/cli/cli/pull/14507",
            "https://github.com/cli/cli/pull/14507/",
            "https://github.com/cli/cli/pull/14507/files",
            "https://github.com/cli/cli/pull/14507/files?w=1#diff-abc",
            "https://GitHub.com/cli/cli/pull/14507/commits/abcdef",
            "http://github.com/cli/cli/pull/14507",
            "  https://github.com/cli/cli/pull/14507\n",
        ] {
            assert_eq!(parse_target(input, no_origin), Ok(want.clone()), "{input}");
        }
    }

    #[test]
    fn ghe_url() {
        assert_eq!(
            parse_target(
                "https://git.corp.example.com/team/svc/pull/7/files",
                no_origin
            ),
            Ok(pr("git.corp.example.com", "team", "svc", 7))
        );
    }

    #[test]
    fn short_slug_form() {
        assert_eq!(
            parse_target("owner/re.po-x#42", no_origin),
            Ok(pr("github.com", "owner", "re.po-x", 42))
        );
    }

    #[test]
    fn number_uses_origin_remote() {
        for (remote, host) in [
            ("git@github.com:cli/cli.git", "github.com"),
            ("git@github.com:cli/cli", "github.com"),
            ("ssh://git@github.com/cli/cli.git", "github.com"),
            ("ssh://git@ssh.github.com:443/cli/cli.git", "github.com"),
            ("https://github.com/cli/cli.git", "github.com"),
            ("https://github.com/cli/cli", "github.com"),
            ("https://user:pw@github.com/cli/cli.git/", "github.com"),
            ("git@ghe.example.com:cli/cli.git", "ghe.example.com"),
            ("ssh://git@ghe.example.com:2222/cli/cli", "ghe.example.com"),
        ] {
            let got = parse_target("123", || Ok(remote.to_string()));
            assert_eq!(got, Ok(pr(host, "cli", "cli", 123)), "{remote}");
        }
        assert_eq!(
            parse_target("#9", || Ok("git@github.com:a/b.git".into())),
            Ok(pr("github.com", "a", "b", 9))
        );
    }

    #[test]
    fn number_without_usable_origin_fails() {
        assert!(parse_target("5", || Err("not a git repo".into())).is_err());
        assert!(parse_target("5", || Ok("/local/path/repo".into())).is_err());
        assert!(parse_target("5", || Ok("https://github.com/only-owner".into())).is_err());
    }

    #[test]
    fn rejects_malformed_targets() {
        for input in [
            "",
            "0",
            "cli/cli",
            "cli/cli#",
            "cli/cli#abc",
            "cli#1",
            "a/b/c#1",
            "../x#1",
            "https://github.com/cli/cli/issues/1",
            "https://github.com/cli/cli/pull/abc",
            "https://github.com/cli/cli",
            "https://github.com/../cli/pull/1",
            "ftp://github.com/cli/cli/pull/1",
        ] {
            assert!(
                parse_target(input, || Err("no origin".into())).is_err(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn web_path() {
        assert_eq!(
            pr("github.com", "cli", "cli", 1).web_path(),
            "/github.com/cli/cli/pull/1"
        );
    }
}
