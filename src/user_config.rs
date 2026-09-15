//! Import extension settings without sharing Codex sessions, databases or login.
use crate::config::ConfigError;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use toml::Value;

const EXTENSIONS: &[&str] = &[
    "mcp_servers",
    "marketplaces",
    "plugins",
    "skills",
    "apps",
    "mcp_oauth_credentials_store",
    "mcp_oauth_callback_port",
    "mcp_oauth_callback_url",
    "mcp_optional_startup_grace_ms",
];
fn io(path: &Path, e: std::io::Error) -> ConfigError {
    ConfigError::Read {
        path: path.into(),
        source: e,
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| io(path, e))
}
fn children(path: &Path) -> Result<Vec<fs::DirEntry>, ConfigError> {
    match fs::read_dir(path) {
        Ok(entries) => entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io(path, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(io(path, e)),
    }
}
fn collect(
    source: &Path,
    prefix: &Path,
    depth: usize,
    links: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), ConfigError> {
    for entry in children(source)? {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let relative = prefix.join(name);
        if depth == 0 {
            links.insert(relative, path);
        } else {
            collect(&path, &relative, depth - 1, links)?;
        }
    }
    Ok(())
}
/// Only remove links which still match our exact recorded targets. User-owned
/// directories, including private skills/plugins, are never replaced or deleted.
fn sync_links(home: &Path, desired: BTreeMap<PathBuf, PathBuf>) -> Result<(), ConfigError> {
    let manifest = home.join(".proxy-extension-links.json");
    let mut old: BTreeMap<PathBuf, PathBuf> = match fs::read(&manifest) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| ConfigError::Invalid("invalid extension link manifest".into()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(io(&manifest, e)),
    };
    let pending_path = home.join(".proxy-extension-links.pending.json");
    let pending: BTreeMap<PathBuf, PathBuf> = match fs::read(&pending_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| ConfigError::Invalid("invalid pending extension manifest".into()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(io(&pending_path, e)),
    };
    for (relative, target) in &pending {
        if fs::read_link(home.join(relative)).ok().as_ref() == Some(target) {
            old.insert(relative.clone(), target.clone());
        }
    }
    for relative in old.keys().chain(desired.keys()).chain(pending.keys()) {
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || !(relative.starts_with("skills")
                || relative.starts_with("plugins/cache")
                || relative.starts_with(".tmp/bundled-marketplaces")
                || relative.starts_with(".tmp/marketplaces"))
        {
            return Err(ConfigError::Invalid(
                "invalid managed extension path".into(),
            ));
        }
    }
    // Refuse symlinked parent directories; no writes may escape the private home.
    for relative in old.keys().chain(desired.keys()) {
        let mut parent = home.to_path_buf();
        for component in relative.parent().unwrap().components() {
            parent.push(component);
            if fs::symlink_metadata(&parent).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(ConfigError::Invalid(format!(
                    "extension parent must not be a symlink: {}",
                    parent.display()
                )));
            }
        }
    }
    let planned: BTreeMap<_, _> = desired
        .iter()
        .filter(|(r, _)| {
            let p = home.join(r);
            fs::symlink_metadata(&p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                || old
                    .get(*r)
                    .is_some_and(|t| fs::read_link(p).ok().as_ref() == Some(t))
        })
        .map(|(r, t)| (r.clone(), t.clone()))
        .collect();
    // Journal before creating links, so deletion after a crash is still tracked.
    atomic(&pending_path, &serde_json::to_vec(&planned).unwrap())?;
    let mut owned = BTreeMap::new();
    for (relative, target) in &old {
        let path = home.join(relative);
        if fs::read_link(&path).ok().as_ref() == Some(target)
            && desired.get(relative) != Some(target)
        {
            fs::remove_file(&path).map_err(|e| io(&path, e))?;
        }
    }
    for (relative, target) in desired {
        let path = home.join(&relative);
        match fs::symlink_metadata(&path) {
            Ok(_) if fs::read_link(&path).ok().as_ref() == Some(&target) => {
                if old.get(&relative) == Some(&target) {
                    owned.insert(relative, target);
                }
            }
            Ok(_) => {
                tracing::warn!(path=%path.display(),"private extension takes precedence over inherited extension");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path.parent().unwrap()).map_err(|e| io(&path, e))?;
                symlink(&target, &path).map_err(|e| io(&path, e))?;
                owned.insert(relative, target);
            }
            Err(e) => return Err(io(&path, e)),
        }
    }
    atomic(&manifest, &serde_json::to_vec(&owned).unwrap())?;
    fs::remove_file(&pending_path).map_err(|e| io(&pending_path, e))?;
    fs::File::open(home)
        .and_then(|f| f.sync_all())
        .map_err(|e| io(home, e))
}
pub fn prepare(home: &Path, source: Option<&Path>, generated: &str) -> Result<(), ConfigError> {
    let mut config: Value = toml::from_str(generated)?;
    let mut desired = BTreeMap::new();
    if let Some(source) = source {
        if !source.is_absolute() {
            return Err(ConfigError::Invalid(
                "codex.user_home must be absolute".into(),
            ));
        }
        // Refuse overlapping homes: generated configuration must never overwrite
        // the source, and extension links must not recurse into private state.
        let source = if source.exists() {
            fs::canonicalize(source).map_err(|e| io(source, e))?
        } else {
            source.into()
        };
        let home_path = fs::canonicalize(home).map_err(|e| io(home, e))?;
        if source.starts_with(&home_path) || home_path.starts_with(&source) {
            return Err(ConfigError::Invalid(
                "Codex user and proxy homes must not overlap".into(),
            ));
        }
        let path = source.join("config.toml");
        let shared: Value = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|_| {
                ConfigError::Invalid(format!(
                    "invalid Codex user configuration: {}",
                    path.display()
                ))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Table(Default::default()),
            Err(e) => return Err(io(&path, e)),
        };
        for key in EXTENSIONS {
            if let Some(value) = shared.get(*key) {
                config
                    .as_table_mut()
                    .unwrap()
                    .insert((*key).into(), value.clone());
            }
        }
        let mut features = toml::map::Map::new();
        for name in [
            "apps",
            "plugins",
            "remote_plugin",
            "skill_search",
            "skip_host_skill_discovery",
        ] {
            if let Some(v) = shared.get("features").and_then(|f| f.get(name)) {
                features.insert(name.into(), v.clone());
            }
        }
        if !features.is_empty() {
            config
                .as_table_mut()
                .unwrap()
                .insert("features".into(), Value::Table(features));
        }
        // Do not inherit global sandbox, approvals, model selection, projects,
        // memories, history or sqlite paths into the managed execution service.
        if let Some(entries) = config
            .get_mut("skills")
            .and_then(|v| v.get_mut("config"))
            .and_then(Value::as_array_mut)
        {
            for entry in entries {
                if let Some(path) = entry.get("path").and_then(Value::as_str) {
                    let p = Path::new(path);
                    if p.is_relative() {
                        entry["path"] = Value::String(source.join(p).to_string_lossy().into());
                    }
                }
            }
        }
        for folder in [".tmp/bundled-marketplaces", ".tmp/marketplaces"] {
            collect(&source.join(folder), Path::new(folder), 0, &mut desired)?;
        }
        if let Some(markets) = config.get_mut("marketplaces").and_then(Value::as_table_mut) {
            for (_, market) in markets.iter_mut() {
                if let Some(path) = market.get("source").and_then(Value::as_str)
                    && let Ok(relative) = Path::new(path).strip_prefix(&source)
                    && (relative.starts_with(".tmp/bundled-marketplaces")
                        || relative.starts_with(".tmp/marketplaces"))
                {
                    market["source"] = Value::String(home.join(relative).to_string_lossy().into());
                }
            }
        }
        collect(&source.join("skills"), Path::new("skills"), 0, &mut desired)?;
        // marketplace / plugin / version. Linking plugin roots keeps updates
        // visible; merging at this level preserves private marketplace installs.
        collect(
            &source.join("plugins/cache"),
            Path::new("plugins/cache"),
            1,
            &mut desired,
        )?;
    }
    fs::set_permissions(home, fs::Permissions::from_mode(0o700)).map_err(|e| io(home, e))?;
    sync_links(home, desired)?;
    let output = toml::to_string(&config)
        .map_err(|_| ConfigError::Invalid("cannot serialize Codex configuration".into()))?;
    atomic(
        &home.join("config.toml"),
        format!("# Generated by codex-hoshikage-proxy. Do not edit.\n{output}").as_bytes(),
    )
}

// MCP reload is applied by Codex at the next active turn. Keep the acknowledged
// snapshot separate from the generated file: a failed/cancelled RPC must retry.
const MCP_KEYS: &[&str] = &[
    "mcp_servers",
    "mcp_oauth_credentials_store",
    "mcp_oauth_callback_port",
    "mcp_oauth_callback_url",
    "mcp_optional_startup_grace_ms",
];
fn mcp_settings(config: &Value) -> Value {
    Value::Table(
        MCP_KEYS
            .iter()
            .filter_map(|k| config.get(*k).map(|v| ((*k).into(), v.clone())))
            .collect(),
    )
}
fn read_config(path: &Path) -> Result<Value, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Table(Default::default()));
        }
        Err(e) => return Err(io(path, e)),
    };
    toml::from_str(&text).map_err(|_| {
        ConfigError::Invalid(format!("invalid Codex configuration: {}", path.display()))
    })
}
pub(crate) struct McpRefresh {
    source: PathBuf,
    generated: PathBuf,
    applied: Value,
    pending: bool,
}
impl McpRefresh {
    pub(crate) fn new(home: &Path, source: Option<&Path>) -> Result<Option<Self>, ConfigError> {
        let Some(source) = source else {
            return Ok(None);
        };
        let generated = home.join("config.toml");
        // Low-level test runtimes may launch without a managed configuration.
        if !generated.exists() {
            return Ok(None);
        }
        Ok(Some(Self {
            source: source.join("config.toml"),
            applied: mcp_settings(&read_config(&generated)?),
            generated,
            pending: false,
        }))
    }
    pub(crate) fn prepare(&mut self) -> Result<Option<Value>, ConfigError> {
        let desired = mcp_settings(&read_config(&self.source)?);
        if desired == self.applied && !self.pending {
            return Ok(None);
        }
        if !self.generated.exists() {
            return Err(ConfigError::Invalid(
                "generated Codex configuration is missing".into(),
            ));
        }
        let mut config = read_config(&self.generated)?;
        for key in MCP_KEYS {
            config.as_table_mut().unwrap().remove(*key);
            if let Some(value) = desired.get(*key) {
                config
                    .as_table_mut()
                    .unwrap()
                    .insert((*key).into(), value.clone());
            }
        }
        let text = toml::to_string(&config)
            .map_err(|_| ConfigError::Invalid("cannot serialize Codex configuration".into()))?;
        self.pending = true;
        atomic(
            &self.generated,
            format!("# Generated by codex-hoshikage-proxy. Do not edit.\n{text}").as_bytes(),
        )?;
        Ok(Some(desired))
    }
    pub(crate) fn acknowledge(&mut self, applied: Value) {
        self.applied = applied;
        self.pending = false;
    }
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    #[test]
    fn unacknowledged_reload_then_source_revert_restores_disk() {
        let root = std::env::temp_dir().join(format!("mcp-revert-{}", uuid::Uuid::new_v4()));
        let source = root.join("user");
        let home = root.join("private");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(source.join("config.toml"), "").unwrap();
        fs::write(home.join("config.toml"), "sandbox_mode='workspace-write'").unwrap();
        let mut refresh = McpRefresh::new(&home, Some(&source)).unwrap().unwrap();
        fs::write(
            source.join("config.toml"),
            "[mcp_servers.new]\ncommand='test'\n",
        )
        .unwrap();
        assert!(refresh.prepare().unwrap().is_some());
        // Simulate cancellation/unknown RPC result, followed by reverting source.
        fs::write(source.join("config.toml"), "").unwrap();
        let reverted = refresh.prepare().unwrap().unwrap();
        assert!(
            read_config(&home.join("config.toml"))
                .unwrap()
                .get("mcp_servers")
                .is_none()
        );
        refresh.acknowledge(reverted);
        assert!(refresh.prepare().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
