use serde::Deserialize;
use std::{
    collections::HashMap,
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
    pub defaults: RawDefaultsConfig,
    pub providers: HashMap<String, RawProviderConfig>,
    pub models: HashMap<String, RawModelConfig>,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            server: RawServerConfig::default(),
            codex: RawCodexConfig::default(),
            security: RawSecurityConfig::default(),
            defaults: RawDefaultsConfig::default(),
            providers: RawModelRegistryConfig::default().providers,
            models: RawModelRegistryConfig::default().models,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawServerConfig {
    pub host: String,
    pub port: u16,
    pub default_cwd: Option<String>,
}

impl Default for RawServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 4040,
            default_cwd: None,
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
pub struct RawModelRegistryConfig {
    pub default_model: String,
    pub providers: HashMap<String, RawProviderConfig>,
    pub models: HashMap<String, RawModelConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawDefaultsConfig {
    pub model: String,
}

impl Default for RawDefaultsConfig {
    fn default() -> Self {
        Self {
            model: "hoshikage/unsloth-gemma4-12b-qat-thinking-off".into(),
        }
    }
}

impl Default for RawModelRegistryConfig {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert("hoshikage".into(), RawProviderConfig::default());
        let mut models = HashMap::new();
        models.insert(
            "hoshikage/unsloth-gemma4-12b-qat-thinking-off".into(),
            RawModelConfig::default(),
        );
        Self {
            default_model: "hoshikage/unsloth-gemma4-12b-qat-thinking-off".into(),
            providers,
            models,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawProviderConfig {
    pub codex_id: String,
    pub enabled: bool,
    pub max_concurrent_turns: usize,
    pub base_url: Option<String>,
    pub auth_env_key: Option<String>,
}

impl Default for RawProviderConfig {
    fn default() -> Self {
        Self {
            codex_id: "hoshikage".into(),
            enabled: true,
            max_concurrent_turns: 1,
            base_url: Some("http://127.0.0.1:3030/v1".into()),
            auth_env_key: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawModelConfig {
    #[serde(alias = "provider")]
    pub provider_id: String,
    #[serde(alias = "upstream_model")]
    pub upstream_id: String,
    pub display_name: String,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
}

impl Default for RawModelConfig {
    fn default() -> Self {
        Self {
            provider_id: "hoshikage".into(),
            upstream_id: "unsloth-gemma4-12b-qat-thinking-off".into(),
            display_name: "Unsloth Gemma 4 12B (thinking off)".into(),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
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
    pub default_cwd: PathBuf,
    pub minimum_codex_version: String,
    pub tested_codex_version: String,
    pub models: RawModelRegistryConfig,
    pub codex_home: PathBuf,
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
        let default_cwd = match raw.server.default_cwd {
            Some(path) => CwdPolicy {
                allowed_roots: roots.clone(),
            }
            .validate(expand_home(&path))?,
            None => roots.first().cloned().ok_or_else(|| {
                ConfigError::Invalid("security.allowed_cwds must not be empty".into())
            })?,
        };
        let registry = RawModelRegistryConfig {
            default_model: raw.defaults.model,
            providers: raw.providers,
            models: raw.models,
        };
        Ok(Self {
            listen_addr,
            codex_command: raw.codex.command,
            codex_args: raw.codex.args,
            cwd_policy: CwdPolicy {
                allowed_roots: roots,
            },
            default_cwd,
            minimum_codex_version: raw.codex.compatibility.minimum_version,
            tested_codex_version: raw.codex.compatibility.tested_version,
            models: registry,
            codex_home: proxy_home().join("codex-home"),
        })
    }

    pub fn prepare_codex_home(&self) -> Result<(), ConfigError> {
        fs::create_dir_all(&self.codex_home).map_err(|source| ConfigError::Read {
            path: self.codex_home.clone(),
            source,
        })?;
        let mut content = String::from("# Generated by codex-hoshikage-proxy. Do not edit.\n\n");
        for (public_id, provider) in &self.models.providers {
            if !provider.enabled {
                continue;
            }
            if matches!(provider.codex_id.as_str(), "openai" | "ollama") {
                // Codex owns these built-in providers and rejects attempts to
                // override them through model_providers.
                continue;
            }
            let Some(base_url) = provider.base_url.as_deref() else {
                continue;
            };
            let name = toml_string(public_id);
            content.push_str(&format!(
                "[model_providers.{public_id}]\nname = {name}\nbase_url = {}\nwire_api = \"responses\"\n",
                toml_string(base_url),
            ));
            if let Some(env_key) = provider
                .auth_env_key
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                content.push_str(&format!("env_key = {}\n", toml_string(env_key)));
            }
            content.push('\n');
        }
        fs::write(self.codex_home.join("config.toml"), content).map_err(|source| {
            ConfigError::Read {
                path: self.codex_home.join("config.toml"),
                source,
            }
        })?;
        Ok(())
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn proxy_home() -> PathBuf {
    env::var("CODEX_HOSHIKAGE_PROXY_HOME")
        .map(PathBuf::from)
        .or_else(|_| {
            env::var("HOME").map(|home| PathBuf::from(home).join(".config/codex-hoshikage-proxy"))
        })
        .unwrap_or_else(|_| PathBuf::from(".codex-hoshikage-proxy"))
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

    #[test]
    fn generates_codex_provider_config_without_touching_auth() {
        let mut raw = RawConfig::default();
        raw.server.default_cwd = Some("/tmp".into());
        raw.security.allowed_cwds = vec!["/tmp".into()];
        let mut config = ValidatedConfig::from_raw(raw).expect("valid test config");
        config.codex_home =
            std::env::temp_dir().join(format!("codex-hoshikage-proxy-test-{}", std::process::id()));
        config
            .prepare_codex_home()
            .expect("provider config generated");
        let generated = fs::read_to_string(config.codex_home.join("config.toml")).unwrap();
        assert!(generated.contains("[model_providers.hoshikage]"));
        assert!(!generated.contains("env_key"));
        assert!(!config.codex_home.join("auth.json").exists());
    }

    #[test]
    fn generates_auth_env_key_only_when_configured() {
        let mut raw = RawConfig::default();
        raw.server.default_cwd = Some("/tmp".into());
        raw.security.allowed_cwds = vec!["/tmp".into()];
        raw.providers
            .get_mut("hoshikage")
            .expect("default provider")
            .auth_env_key = Some("HOSHIKAGE_API_KEY".into());
        let mut config = ValidatedConfig::from_raw(raw).expect("valid test config");
        config.codex_home = std::env::temp_dir().join(format!(
            "codex-hoshikage-proxy-auth-test-{}",
            std::process::id()
        ));
        config
            .prepare_codex_home()
            .expect("provider config generated");
        let generated = fs::read_to_string(config.codex_home.join("config.toml")).unwrap();
        assert!(generated.contains("env_key = \"HOSHIKAGE_API_KEY\""));
    }

    #[test]
    fn does_not_override_codex_builtin_providers() {
        let mut raw = RawConfig::default();
        raw.server.default_cwd = Some("/tmp".into());
        raw.security.allowed_cwds = vec!["/tmp".into()];
        raw.providers.insert(
            "ollama".into(),
            RawProviderConfig {
                codex_id: "ollama".into(),
                enabled: true,
                max_concurrent_turns: 1,
                base_url: Some("http://127.0.0.1:11434/v1".into()),
                auth_env_key: None,
            },
        );
        let mut config = ValidatedConfig::from_raw(raw).expect("valid test config");
        config.codex_home = std::env::temp_dir().join(format!(
            "codex-hoshikage-proxy-builtin-test-{}",
            std::process::id()
        ));
        config
            .prepare_codex_home()
            .expect("provider config generated");
        let generated = fs::read_to_string(config.codex_home.join("config.toml")).unwrap();
        assert!(!generated.contains("[model_providers.ollama]"));
    }
}
