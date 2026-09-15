use codex_hoshikage_proxy::config::{RawConfig, ValidatedConfig};
use std::{fs, os::unix::fs::PermissionsExt};
fn setup() -> (std::path::PathBuf, ValidatedConfig) {
    let root = std::env::temp_dir().join(format!("extension-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("user/skills/shared")).unwrap();
    fs::create_dir_all(root.join("user/plugins/cache/market/shared/1")).unwrap();
    fs::create_dir_all(root.join("proxy/skills/private")).unwrap();
    let mut raw = RawConfig::default();
    raw.security.api_key = Some("test".into());
    raw.server.default_cwd = Some(root.to_string_lossy().into());
    raw.security.allowed_cwds = vec![root.to_string_lossy().into()];
    raw.codex.user_home = Some(root.join("user"));
    let mut c = ValidatedConfig::from_raw(raw).unwrap();
    c.codex_home = root.join("proxy");
    (root, c)
}
#[test]
fn inherits_extensions_and_updates_without_sharing_runtime_state() {
    let (root, c) = setup();
    let source = r#"model="global-model"
sandbox_mode="danger-full-access"
[features]
memories=true
[mcp_servers.test]
command="test-mcp"
[mcp_servers.test.env]
TOKEN="secret-example"
[plugins."shared@market"]
enabled=false
[marketplaces.market]
source_type="local"
source="/tmp/catalog"
[[skills.config]]
path="skills/shared"
enabled=false
"#;
    fs::write(root.join("user/config.toml"), source).unwrap();
    fs::write(root.join("proxy/auth.json"), "private-auth").unwrap();
    fs::write(root.join("user/auth.json"), "global-auth").unwrap();
    c.prepare_codex_home().unwrap();
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("proxy/config.toml")).unwrap()).unwrap();
    assert_eq!(config["sandbox_mode"].as_str(), Some("workspace-write"));
    assert!(config.get("model").is_none() && config.get("features").is_none());
    assert_eq!(
        config["mcp_servers"]["test"]["env"]["TOKEN"].as_str(),
        Some("secret-example")
    );
    assert_eq!(
        config["plugins"]["shared@market"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        config["skills"]["config"][0]["path"].as_str(),
        root.join("user/skills/shared").to_str()
    );
    assert_eq!(
        fs::read_link(root.join("proxy/skills/shared")).unwrap(),
        root.join("user/skills/shared")
    );
    assert!(root.join("proxy/plugins/cache/market/shared/1").exists());
    assert!(root.join("proxy/skills/private").is_dir());
    assert!(!root.join("proxy/sessions").exists());
    assert_eq!(
        fs::read_to_string(root.join("proxy/auth.json")).unwrap(),
        "private-auth"
    );
    assert_eq!(
        fs::read_to_string(root.join("user/config.toml")).unwrap(),
        source
    );
    assert_eq!(
        fs::metadata(root.join("proxy/config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::write(root.join("user/config.toml"), "").unwrap();
    fs::remove_dir_all(root.join("user/skills/shared")).unwrap();
    c.prepare_codex_home().unwrap();
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("proxy/config.toml")).unwrap()).unwrap();
    assert!(config.get("mcp_servers").is_none());
    assert!(fs::symlink_metadata(root.join("proxy/skills/shared")).is_err());
    assert!(root.join("proxy/skills/private").is_dir());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_source_keeps_previous_config_and_disabling_preserves_private_skills() {
    let (root, mut c) = setup();
    fs::write(root.join("user/config.toml"), "").unwrap();
    c.prepare_codex_home().unwrap();
    let before = fs::read(root.join("proxy/config.toml")).unwrap();
    fs::write(root.join("user/config.toml"), "token='secret-example\n").unwrap();
    let e = c.prepare_codex_home().unwrap_err().to_string();
    assert!(!e.contains("secret-example"));
    assert_eq!(fs::read(root.join("proxy/config.toml")).unwrap(), before);
    c.codex_user_home = None;
    c.prepare_codex_home().unwrap();
    assert!(!root.join("proxy/skills/shared").exists());
    assert!(root.join("proxy/skills/private").exists());
    c.codex_user_home = Some(c.codex_home.clone());
    assert!(c.prepare_codex_home().is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_tracks_only_owned_links_and_rejects_symlink_parents() {
    use std::os::unix::fs::symlink;
    let (root, c) = setup();
    fs::write(root.join("user/config.toml"), "").unwrap();
    // An existing user-managed link must not be adopted and later deleted.
    symlink(
        root.join("user/skills/shared"),
        root.join("proxy/skills/shared"),
    )
    .unwrap();
    c.prepare_codex_home().unwrap();
    fs::remove_dir_all(root.join("user/skills/shared")).unwrap();
    c.prepare_codex_home().unwrap();
    assert!(
        fs::symlink_metadata(root.join("proxy/skills/shared"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // Reproduce a crash after creating a recorded link, before final manifest commit.
    let target = root.join("user/skills/gone");
    symlink(&target, root.join("proxy/skills/half-created")).unwrap();
    fs::write(
        root.join("proxy/.proxy-extension-links.pending.json"),
        serde_json::to_vec(&serde_json::json!({"skills/half-created":target})).unwrap(),
    )
    .unwrap();
    c.prepare_codex_home().unwrap();
    assert!(fs::symlink_metadata(root.join("proxy/skills/half-created")).is_err());
    fs::rename(root.join("proxy/skills"), root.join("kept-skills")).unwrap();
    symlink(root.join("kept-skills"), root.join("proxy/skills")).unwrap();
    fs::create_dir_all(root.join("user/skills/new")).unwrap();
    assert!(c.prepare_codex_home().is_err());
    assert!(!root.join("kept-skills/new").exists());
    fs::remove_dir_all(root).unwrap();
}
