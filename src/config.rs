use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_PORT: u16 = 7457;

/// `$XDG_CONFIG_HOME/tideline/config.toml`; every field is optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub ghe_hosts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: DEFAULT_PORT,
            ghe_hosts: Vec::new(),
        }
    }
}

impl Config {
    /// Hosts allowed in `/api/{host}/...`: github.com plus configured GHE hosts.
    pub fn allowed_hosts(&self) -> Vec<String> {
        let mut hosts = vec!["github.com".to_string()];
        for h in &self.ghe_hosts {
            let h = h.trim().to_ascii_lowercase();
            if !h.is_empty() && !hosts.contains(&h) {
                hosts.push(h);
            }
        }
        hosts
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Paths, String> {
        let dirs = directories::ProjectDirs::from("", "", "tideline")
            .ok_or("cannot determine home directory")?;
        Ok(Paths {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// `server-<port>.json`: one per running server, so servers on different ports do not
    /// clobber each other's file.
    pub fn server_json(&self, port: u16) -> PathBuf {
        self.cache_dir.join(format!("server-{port}.json"))
    }

    pub fn repos_dir(&self) -> PathBuf {
        self.cache_dir.join("repos")
    }
}

pub fn load_config(paths: &Paths) -> Result<Config, String> {
    let file = paths.config_dir.join("config.toml");
    match std::fs::read_to_string(&file) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

fn valid_secret(text: &str) -> Option<String> {
    let text = text.trim();
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// Reads `$XDG_CONFIG_HOME/tideline/secret` (64 hex chars), creating it atomically
/// (`O_CREAT|O_EXCL`, mode 0600) if missing. If another gh-tideline creates it concurrently, the
/// winner's secret is read back. The config dir is created/chmodded to 0700.
pub fn load_or_create_secret(paths: &Paths) -> Result<String, String> {
    let dir = &paths.config_dir;
    crate::git::ensure_private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("secret");
    let err = |e: std::io::Error| format!("{}: {e}", file.display());
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
    {
        Ok(mut f) => {
            let secret = hex::encode(rand::random::<[u8; 32]>());
            f.write_all(secret.as_bytes()).map_err(err)?;
            return Ok(secret);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(err(e)),
    }
    // A concurrent creator may not have written it yet; give it a moment.
    for _ in 0..20 {
        let text = std::fs::read_to_string(&file).map_err(err)?;
        if let Some(secret) = valid_secret(&text) {
            ensure_mode_0600(&file)?;
            return Ok(secret);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err(format!(
        "{}: not a 64-character hex secret; delete it and run gh-tideline again",
        file.display()
    ))
}

fn ensure_mode_0600(file: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if meta.permissions().mode() & 0o777 != 0o600 {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_created_once_with_private_modes() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gh-tideline-test-secret-{nanos}"));
        let paths = Paths {
            config_dir: root.join("config/tideline"),
            cache_dir: root.join("cache"),
        };
        let a = load_or_create_secret(&paths).unwrap();
        let b = load_or_create_secret(&paths).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths.config_dir.join("secret")), 0o600);
        assert_eq!(mode(&paths.config_dir), 0o700);
        std::fs::write(paths.config_dir.join("secret"), "garbage").unwrap();
        assert!(load_or_create_secret(&paths).is_err());
        assert_eq!(paths.server_json(7464), root.join("cache/server-7464.json"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
