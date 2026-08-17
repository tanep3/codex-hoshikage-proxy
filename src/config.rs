use serde::Deserialize;
use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawConfig {
    pub server: RawServerConfig,
    pub codex: RawCodexConfig,
    pub security: RawSecurityConfig,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            server: RawServerConfig::default(),
            codex: RawCodexConfig::default(),
            security: RawSecurityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for RawServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 4040,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawCodexConfig {
    pub command: String,
    pub args: Vec<String>,
    pub compatibility: RawCompatibilityConfig,
}

impl Default for RawCodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".into(),
            args: vec!["app-server".into(), "--listen".into(), "stdio://".into()],
            compatibility: RawCompatibilityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawSecurityConfig {
    pub allowed_cwds: Vec<String>,
}

impl Default for RawSecurityConfig {
    fn default() -> Self {
        Self {
            allowed_cwds: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawCompatibilityConfig {
    pub minimum_version: String,
    pub tested_version: String,
}

impl Default for RawCompatibilityConfig {
    fn default() -> Self {
        Self {
            minimum_version: "0.147.0".into(),
            tested_version: "0.147.0".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidatedConfig {
    pub listen_addr: SocketAddr,
    pub codex_command: String,
    pub codex_args: Vec<String>,
    pub cwd_policy: CwdPolicy,
    pub minimum_codex_version: String,
    pub tested_codex_version: String,
}

#[derive(Debug, Clone)]
pub struct CwdPolicy {
    allowed_roots: Vec<PathBuf>,
}

impl CwdPolicy {
    pub fn validate(&self, cwd: impl AsRef<Path>) -> Result<PathBuf, ConfigError> {
        let cwd = cwd.as_ref();
        if !cwd.is_absolute() {
            return Err(ConfigError::Invalid("cwd must be absolute".into()));
        }
        let canonical = cwd.canonicalize().map_err(|error| {
            ConfigError::Invalid(format!("cwd does not exist: {cwd:?}: {error}"))
        })?;
        if self
            .allowed_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            Ok(canonical)
        } else {
            Err(ConfigError::Invalid(format!(
                "cwd is outside the allowlist: {canonical:?}"
            )))
        }
    }
}

impl ValidatedConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();
        let content = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let raw: RawConfig = toml::from_str(&content)?;
        Self::from_raw(raw)
    }

    pub fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let listen_addr = format!("{}:{}", expand_home(&raw.server.host), raw.server.port)
            .parse()
            .map_err(|error| ConfigError::Invalid(format!("invalid server address: {error}")))?;
        if raw.codex.command.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "codex.command must not be empty".into(),
            ));
        }
        if raw.security.allowed_cwds.is_empty() {
            return Err(ConfigError::Invalid(
                "security.allowed_cwds must not be empty".into(),
            ));
        }
        let mut roots = Vec::new();
        for configured in raw.security.allowed_cwds {
            let path = PathBuf::from(expand_home(&configured));
            if !path.is_absolute() {
                return Err(ConfigError::Invalid(format!(
                    "allowed cwd must be absolute: {path:?}"
                )));
            }
            roots.push(path.canonicalize().map_err(|error| {
                ConfigError::Invalid(format!("allowed cwd does not exist: {path:?}: {error}"))
            })?);
        }
        Ok(Self {
            listen_addr,
            codex_command: raw.codex.command,
            codex_args: raw.codex.args,
            cwd_policy: CwdPolicy {
                allowed_roots: roots,
            },
            minimum_codex_version: raw.codex.compatibility.minimum_version,
            tested_codex_version: raw.codex.compatibility.tested_version,
        })
    }
}

pub fn default_config_path() -> PathBuf {
    if let Ok(path) = env::var("CODEX_HOSHIKAGE_PROXY_CONFIG") {
        return PathBuf::from(path);
    }
    let root = env::var("CODEX_HOSHIKAGE_PROXY_HOME")
        .map(PathBuf::from)
        .or_else(|_| {
            env::var("HOME").map(|home| PathBuf::from(home).join(".config/codex-hoshikage-proxy"))
        })
        .unwrap_or_else(|_| PathBuf::from(".codex-hoshikage-proxy"));
    root.join("config.toml")
}

fn expand_home(value: &str) -> String {
    let home = env::var("HOME").unwrap_or_default();
    value.replace("${HOME}", &home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_allowlist() {
        let mut raw = RawConfig::default();
        raw.security.allowed_cwds.clear();
        assert!(ValidatedConfig::from_raw(raw).is_err());
    }

    #[test]
    fn compatibility_is_nested_under_codex() {
        let raw: RawConfig = toml::from_str(
            r#"
            [server]
            host = "127.0.0.1"
            port = 4040

            [codex]
            command = "codex"
            args = ["app-server", "--listen", "stdio://"]

            [codex.compatibility]
            minimum_version = "0.147.0"
            tested_version = "0.147.0"

            [security]
            allowed_cwds = ["/tmp"]
            "#,
        )
        .expect("valid config");

        let config = ValidatedConfig::from_raw(raw).expect("existing allowlist root");
        assert_eq!(config.minimum_codex_version, "0.147.0");
        assert_eq!(config.tested_codex_version, "0.147.0");
    }
}
