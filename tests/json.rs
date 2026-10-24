//! JSON format, shared-file namespace preservation and compare-and-set saves.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use configurable_cli::{AppConfig, ConfigError, ConfigFormat, ConfigManager, ConfigSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A host that owns only two namespaces of a file another program also writes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Namespaces {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control: Option<Control>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    targets: BTreeMap<String, Target>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Control {
    enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Target {
    ssh: String,
}

impl AppConfig for Namespaces {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.targets.keys().any(String::is_empty) {
            return Err(ConfigError::validation("target names must not be empty"));
        }
        Ok(())
    }
}

fn shared() -> ConfigManager<Namespaces> {
    ConfigManager::new(
        ConfigSpec::new("tyler")
            .with_file_name("config.json")
            .preserving_unknown_keys(),
    )
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// What another program (e.g. a native app) keeps in the same file.
fn foreign() -> Value {
    json!({
        "profiles": [{"id": "A", "name": "Default", "hue": 210}],
        "preferences": {"theme": {"accent": "#E2A64C"}, "gutter": 6},
        "plugins": {"culture": {"enabled": true, "channel": "stable"}},
        "control": {"enabled": true},
        "targets": {"office": {"ssh": "office.example"}}
    })
}

#[test]
fn json_file_names_select_json_and_yaml_stays_the_default() {
    assert_eq!(ConfigSpec::new("x").format, ConfigFormat::Yaml);
    assert_eq!(
        ConfigSpec::new("x").with_file_name("config.json").format,
        ConfigFormat::Json
    );
    assert_eq!(
        ConfigSpec::new("x").with_file_name("settings.yml").format,
        ConfigFormat::Yaml
    );
    assert_eq!(
        ConfigSpec::new("x")
            .with_file_name("config.json")
            .with_format(ConfigFormat::Yaml)
            .format,
        ConfigFormat::Yaml
    );
    assert_eq!(
        ConfigSpec::new("x").with_file_name("config").format,
        ConfigFormat::Yaml
    );
}

#[test]
fn json_round_trips_and_reports_json_parse_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let manager =
        ConfigManager::<Namespaces>::new(ConfigSpec::new("tyler").with_file_name("config.json"));
    let config = Namespaces {
        control: Some(Control { enabled: false }),
        targets: BTreeMap::new(),
    };
    manager.save(&path, &config).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("{\n") && text.ends_with("}\n"),
        "pretty JSON with a trailing newline: {text}"
    );
    assert_eq!(manager.load(Some(&path)).unwrap().config, config);
    assert!(manager.export(Some(&path)).unwrap().contains("\"control\""));
    fs::write(&path, "control:\n  enabled: true\n").unwrap();
    let error = manager.load(Some(&path)).unwrap_err();
    assert!(matches!(error, ConfigError::ParseJson { .. }), "{error:?}");
}

#[test]
fn saves_keep_every_namespace_the_host_does_not_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, serde_json::to_string_pretty(&foreign()).unwrap()).unwrap();
    let manager = shared();
    let mut loaded = manager.load(Some(&path)).unwrap().config;
    assert_eq!(loaded.targets["office"].ssh, "office.example");
    loaded.control = Some(Control { enabled: false });
    loaded.targets.insert(
        "lab".into(),
        Target {
            ssh: "lab.example".into(),
        },
    );
    manager.save(&path, &loaded).unwrap();
    let after = read(&path);
    let before = foreign();
    for key in ["profiles", "preferences", "plugins"] {
        assert_eq!(after[key], before[key], "{key} must survive untouched");
    }
    assert_eq!(after["control"], json!({"enabled": false}));
    assert_eq!(after["targets"]["lab"], json!({"ssh": "lab.example"}));
}

#[test]
fn owned_keys_the_host_no_longer_serializes_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, foreign().to_string()).unwrap();
    shared().save(&path, &Namespaces::default()).unwrap();
    let after = read(&path);
    assert!(
        after.get("control").is_none() && after.get("targets").is_none(),
        "{after}"
    );
    assert_eq!(after["plugins"], foreign()["plugins"]);
}

#[test]
fn replacing_without_preservation_drops_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, foreign().to_string()).unwrap();
    let manager =
        ConfigManager::<Namespaces>::new(ConfigSpec::new("tyler").with_file_name("config.json"));
    manager.save(&path, &Namespaces::default()).unwrap();
    assert_eq!(read(&path), json!({}));
}

#[test]
fn a_foreign_document_that_does_not_parse_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, "{ not json").unwrap();
    let error = shared().save(&path, &Namespaces::default()).unwrap_err();
    assert!(matches!(error, ConfigError::ParseJson { .. }), "{error:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn invalid_values_never_reach_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, foreign().to_string()).unwrap();
    let mut bad = Namespaces::default();
    bad.targets.insert(String::new(), Target::default());
    assert!(matches!(
        shared().save(&path, &bad),
        Err(ConfigError::Validation(_))
    ));
    assert_eq!(read(&path), foreign());
}

#[test]
fn compare_and_set_refuses_concurrent_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let manager = shared();
    let absent = manager.load(Some(&path)).unwrap();
    assert!(absent.revision.is_none() && !absent.existed);
    manager
        .save_if_revision(&path, &Namespaces::default(), None)
        .unwrap();
    let loaded = manager.load(Some(&path)).unwrap();
    let revision = loaded.revision.clone().unwrap();
    assert!(revision.starts_with("sha256:"));
    // Another writer changes the file after our load.
    fs::write(&path, foreign().to_string()).unwrap();
    let error = manager
        .save_if_revision(&path, &loaded.config, Some(&revision))
        .unwrap_err();
    assert!(matches!(error, ConfigError::Conflict(_)), "{error:?}");
    assert_eq!(read(&path), foreign());
    // Reloading picks up their change; a save at the new revision succeeds.
    let fresh = manager.load(Some(&path)).unwrap();
    manager
        .save_if_revision(&path, &fresh.config, fresh.revision.as_deref())
        .unwrap();
    assert_eq!(read(&path)["plugins"], foreign()["plugins"]);
    assert!(
        manager
            .save_if_revision(&path, &fresh.config, None)
            .is_err(),
        "None means the file must not exist"
    );
}

#[cfg(unix)]
#[test]
fn json_saves_follow_managed_symlink_chains_and_stay_owner_only() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let config = dir.path().join("config/tyler");
    fs::create_dir_all(&store).unwrap();
    fs::create_dir_all(&config).unwrap();
    let real = store.join("tyler.json");
    fs::write(&real, foreign().to_string()).unwrap();
    symlink("../store/tyler.json", dir.path().join("config/hop.json")).unwrap();
    symlink("../hop.json", config.join("config.json")).unwrap();
    let path = config.join("config.json");
    let manager = shared();
    let mut loaded = manager.load(Some(&path)).unwrap().config;
    loaded.control = Some(Control { enabled: false });
    manager.save(&path, &loaded).unwrap();
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "user-facing link kept"
    );
    assert!(
        fs::symlink_metadata(dir.path().join("config/hop.json"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "intermediate link kept"
    );
    assert_eq!(read(&real)["control"], json!({"enabled": false}));
    assert_eq!(read(&real)["plugins"], foreign()["plugins"]);
    assert_eq!(
        fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let leftovers: Vec<_> = fs::read_dir(&store)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
