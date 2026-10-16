//! The list of servers and how to authenticate to each.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// One configured server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_cmd: Option<String>,
}

/// What the client presents to the server.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Login and password, exchanged for a session token that can be renewed.
    Password { login: String, password: String },
    /// A personal access token, used as is.
    Token(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "server")]
    servers: Vec<ServerConfig>,
}

impl ServerConfig {
    /// Resolves the secret, running its command when one is configured.
    ///
    /// # Errors
    /// Fails when the secret is missing, given twice, or its command fails.
    pub fn credentials(&self) -> Result<Credentials> {
        let secrets = [
            &self.password,
            &self.password_cmd,
            &self.token,
            &self.token_cmd,
        ];
        if secrets.iter().filter(|s| s.is_some()).count() != 1 {
            bail!(
                "server {}: set exactly one of password, password_cmd, token, token_cmd",
                self.name
            );
        }
        match (
            &self.password,
            &self.password_cmd,
            &self.token,
            &self.token_cmd,
        ) {
            (Some(password), ..) => self.password_of(password.clone()),
            (_, Some(cmd), ..) => self.password_of(run(cmd)?),
            (_, _, Some(token), _) => Ok(Credentials::Token(token.clone())),
            (.., Some(cmd)) => Ok(Credentials::Token(run(cmd)?)),
            _ => bail!("server {}: no credentials", self.name),
        }
    }

    fn password_of(&self, password: String) -> Result<Credentials> {
        if self.login.is_empty() {
            bail!("server {}: a password needs a login", self.name);
        }
        Ok(Credentials::Password {
            login: self.login.clone(),
            password,
        })
    }
}

fn run(cmd: &str) -> Result<String> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .context("running credential command")?;
    if !out.status.success() {
        bail!("credential command failed");
    }
    Ok(String::from_utf8(out.stdout)
        .context("credential command output is not UTF-8")?
        .trim_end()
        .to_owned())
}

/// Where the server list lives.
///
/// # Errors
/// Fails when no home directory can be found.
pub fn default_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "mm").context("no home directory")?;
    Ok(dirs.config_dir().join("servers.toml"))
}

/// Reads the server list; a missing file is an empty list.
///
/// # Errors
/// Fails when the file cannot be read or parsed.
pub fn load(path: &Path) -> Result<Vec<ServerConfig>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(toml::from_str::<File>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
            .servers),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes the server list readable by its owner only.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn save(path: &Path, servers: &[ServerConfig]) -> Result<()> {
    let text = toml::to_string(&File {
        servers: servers.to_vec(),
    })?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn server() -> ServerConfig {
        ServerConfig {
            name: "work".into(),
            url: "https://chat.example.org".into(),
            login: "paul".into(),
            password: Some("hunter2".into()),
            password_cmd: None,
            token: None,
            token_cmd: None,
        }
    }

    #[test]
    fn round_trip_keeps_owner_only_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mm/servers.toml");
        save(&path, &[server()]).unwrap();
        assert_eq!(load(&path).unwrap(), vec![server()]);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("none.toml")).unwrap().is_empty());
    }

    #[test]
    fn two_secrets_are_rejected() {
        let mut s = server();
        s.password_cmd = Some("echo x".into());
        assert!(s.credentials().is_err());
    }

    #[test]
    fn password_cmd_is_run() {
        let mut s = server();
        s.password = None;
        s.password_cmd = Some("echo secret".into());
        match s.credentials().unwrap() {
            Credentials::Password { password, .. } => assert_eq!(password, "secret"),
            Credentials::Token(_) => panic!("expected a password"),
        }
    }

    #[test]
    fn token_needs_no_login() {
        let s = ServerConfig {
            login: String::new(),
            password: None,
            token: Some("abc".into()),
            ..server()
        };
        assert!(matches!(s.credentials().unwrap(), Credentials::Token(t) if t == "abc"));
    }
}
