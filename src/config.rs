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
    pub approval: RawApprovalConfig,
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
            approval: RawApprovalConfig::default(),
            defaults: RawDefaultsConfig::default(),
            providers: RawModelRegistryConfig::default().providers,
            models: RawModelRegistryConfig::default().models,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawApprovalConfig {
    pub timeout_seconds: u64,
    pub auto_approve_workspace: bool,
}

impl Default for RawApprovalConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            auto_approve_workspace: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RawServerConfig {
    pub host: String,
    pub port: u16,
    pub default_cwd: Option<String>,
    pub turn_stall_detection_seconds: u64,
    pub turn_stall_confirmation_count: u32,
    pub turn_heartbeat_seconds: u64,
    pub turn_idle_timeout_seconds: u64,
}

impl Default for RawServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 4040,
            default_cwd: None,
            turn_stall_detection_seconds: 180,
            turn_stall_confirmation_count: 3,
            turn_heartbeat_seconds: 30,
            turn_idle_timeout_seconds: 600,
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawSecurityConfig {
    pub allowed_cwds: Vec<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
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
            model: "chatgpt/gpt-5.6-luna".into(),
        }
    }
}

impl Default for RawModelRegistryConfig {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert("hoshikage".into(), RawProviderConfig::hoshikage_default());
        providers.insert(
            "chatgpt".into(),
            RawProviderConfig {
                codex_id: "openai".into(),
                enabled: true,
                max_concurrent_turns: 4,
                base_url: None,
                auth_env_key: None,
            },
        );
        Self {
            default_model: "chatgpt/gpt-5.6-luna".into(),
            providers,
            models: HashMap::new(),
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

impl RawProviderConfig {
    fn hoshikage_default() -> Self {
        Self {
            codex_id: "hoshikage".into(),
            enabled: true,
            max_concurrent_turns: 1,
            base_url: Some("http://127.0.0.1:3030/v1".into()),
            auth_env_key: None,
        }
    }
}

impl Default for RawProviderConfig {
    fn default() -> Self {
        Self {
            codex_id: String::new(),
            enabled: false,
            max_concurrent_turns: 1,
            base_url: None,
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
    pub api_key: Option<String>,
    pub approval_timeout_seconds: u64,
    pub auto_approve_workspace: bool,
    pub turn_idle_timeout_seconds: u64,
    pub turn_stall_detection_seconds: u64,
    pub turn_stall_confirmation_count: u32,
    pub turn_heartbeat_seconds: u64,
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
        if raw.server.turn_stall_detection_seconds == 0 {
            return Err(ConfigError::Invalid(
                "server.turn_stall_detection_seconds must be greater than zero".into(),
            ));
        }
        if raw.server.turn_stall_confirmation_count == 0 {
            return Err(ConfigError::Invalid(
                "server.turn_stall_confirmation_count must be greater than zero".into(),
            ));
        }
        if raw.server.turn_heartbeat_seconds == 0 {
            return Err(ConfigError::Invalid(
                "server.turn_heartbeat_seconds must be greater than zero".into(),
            ));
        }
        if raw.server.turn_idle_timeout_seconds == 0 {
            return Err(ConfigError::Invalid(
                "server.turn_idle_timeout_seconds must be greater than zero".into(),
            ));
        }
        Self::from_raw(raw)
    }

    pub fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let listen_addr: SocketAddr =
            format!("{}:{}", expand_home(&raw.server.host), raw.server.port)
                .parse()
                .map_err(|error| {
                    ConfigError::Invalid(format!("invalid server address: {error}"))
                })?;
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
        let api_key = raw
            .security
            .api_key
            .filter(|value| !value.is_empty())
            .or_else(|| {
                raw.security
                    .api_key_env
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .and_then(|name| env::var(name).ok())
                    .filter(|value| !value.is_empty())
            });
        if !listen_addr.ip().is_loopback() && api_key.is_none() {
            return Err(ConfigError::Invalid(
                "non-loopback server requires security.api_key_env with a non-empty environment value".into(),
            ));
        }
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
            api_key,
            approval_timeout_seconds: raw.approval.timeout_seconds,
            auto_approve_workspace: raw.approval.auto_approve_workspace,
            turn_idle_timeout_seconds: raw.server.turn_idle_timeout_seconds,
            turn_stall_detection_seconds: raw.server.turn_stall_detection_seconds,
            turn_stall_confirmation_count: raw.server.turn_stall_confirmation_count,
            turn_heartbeat_seconds: raw.server.turn_heartbeat_seconds,
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
    fn rejects_non_loopback_without_api_key() {
        let mut raw = RawConfig::default();
        raw.server.host = "192.0.2.10".into();
        raw.security.allowed_cwds = vec![
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        let error = ValidatedConfig::from_raw(raw).unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(message) if message.contains("non-loopback")));
    }

    #[test]
    fn accepts_api_key_from_declarative_config() {
        let mut raw = RawConfig::default();
        raw.security.allowed_cwds = vec![
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        raw.security.api_key = Some("config-secret".into());
        let config = ValidatedConfig::from_raw(raw).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("config-secret"));
    }

    #[test]
    fn provider_without_base_url_does_not_inherit_hoshikage_endpoint() {
        let raw: RawConfig = toml::from_str(
            r#"
            [providers.chatgpt]
            codex_id = "openai"
            enabled = true
            max_concurrent_turns = 4
            "#,
        )
        .expect("provider configuration parses");

        let chatgpt = raw.providers.get("chatgpt").expect("chatgpt provider");
        assert_eq!(chatgpt.base_url, None);
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
