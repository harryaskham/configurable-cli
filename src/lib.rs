//! Canonical configuration mixin for the harryaskham Rust CLI stack.
//!
//! A host supplies a serde/schemars configuration type and a [`ConfigSpec`].
//! The crate then owns path precedence, absent-file defaults, semantic
//! validation, atomic owner-only writes, schema generation, import/export,
//! clap subcommands, and matching MCP tools.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use mcp_cli::{ErrorCategory, StructuredError, ToolRouter};
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration values supported by the reusable manager.
///
/// Hosts implement [`validate`](Self::validate) for semantic checks that serde
/// and JSON Schema cannot express. An absent or empty file yields `Default`.
pub trait AppConfig:
    Clone + Default + Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static
{
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}

/// Stable host metadata used for path resolution and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSpec {
    pub app_name: String,
    pub env_var: String,
    pub file_name: String,
}

impl ConfigSpec {
    #[must_use]
    pub fn new(app_name: impl Into<String>) -> Self {
        let app_name = app_name.into();
        let env_var = format!(
            "{}_CONFIG",
            app_name
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        Self {
            app_name,
            env_var,
            file_name: "config.yaml".into(),
        }
    }

    #[must_use]
    pub fn with_env_var(mut self, env_var: impl Into<String>) -> Self {
        self.env_var = env_var.into();
        self
    }

    #[must_use]
    pub fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = file_name.into();
        self
    }
}

/// Loaded config plus provenance required by status/report surfaces.
#[derive(Clone, Debug)]
pub struct Loaded<C> {
    pub path: PathBuf,
    pub existed: bool,
    pub config: C,
}

/// Reusable configuration manager.
#[derive(Clone, Debug)]
pub struct ConfigManager<C> {
    spec: ConfigSpec,
    marker: PhantomData<fn() -> C>,
}

impl<C: AppConfig> ConfigManager<C> {
    #[must_use]
    pub fn new(spec: ConfigSpec) -> Self {
        Self {
            spec,
            marker: PhantomData,
        }
    }

    #[must_use]
    pub fn spec(&self) -> &ConfigSpec {
        &self.spec
    }

    /// `$APP_CONFIG`, then `$XDG_CONFIG_HOME/<app>/config.yaml`, then
    /// `$HOME/.config/<app>/config.yaml`, then a relative `.config` fallback.
    #[must_use]
    pub fn default_path(&self) -> PathBuf {
        if let Some(value) = non_empty_env(&self.spec.env_var) {
            return PathBuf::from(value);
        }
        if let Some(value) = non_empty_env("XDG_CONFIG_HOME") {
            return PathBuf::from(value)
                .join(&self.spec.app_name)
                .join(&self.spec.file_name);
        }
        if let Some(value) = non_empty_env("HOME") {
            return PathBuf::from(value)
                .join(".config")
                .join(&self.spec.app_name)
                .join(&self.spec.file_name);
        }
        PathBuf::from(".config")
            .join(&self.spec.app_name)
            .join(&self.spec.file_name)
    }

    #[must_use]
    pub fn resolve_path(&self, explicit: Option<&Path>) -> PathBuf {
        explicit.map_or_else(|| self.default_path(), Path::to_path_buf)
    }

    pub fn parse(&self, text: &str) -> Result<C, ConfigError> {
        let config = if text.trim().is_empty() {
            C::default()
        } else {
            serde_yaml::from_str(text).map_err(|source| ConfigError::Parse {
                app: self.spec.app_name.clone(),
                source,
            })?
        };
        config.validate()?;
        Ok(config)
    }

    pub fn load(&self, explicit: Option<&Path>) -> Result<Loaded<C>, ConfigError> {
        let path = self.resolve_path(explicit);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Loaded {
                path,
                existed: true,
                config: self.parse(&text)?,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
                path,
                existed: false,
                config: C::default(),
            }),
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    pub fn load_path(&self, path: &Path) -> Result<C, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        self.parse(&text)
    }

    /// Validate before an owner-only atomic replacement. A rejected value never
    /// changes the destination. If `path` is a symlink, the write follows its
    /// complete chain and atomically replaces the real target without unlinking
    /// the user-facing symlink.
    pub fn save(&self, path: &Path, config: &C) -> Result<(), ConfigError> {
        config.validate()?;
        let yaml = serde_yaml::to_string(config).map_err(ConfigError::Encode)?;
        atomic_write(path, yaml.as_bytes())
    }

    pub fn init(&self, path: &Path, force: bool) -> Result<C, ConfigError> {
        if path.exists() && !force {
            return Err(ConfigError::Exists(path.to_path_buf()));
        }
        let config = C::default();
        self.save(path, &config)?;
        Ok(config)
    }

    pub fn import(&self, source: &Path, destination: &Path, force: bool) -> Result<C, ConfigError> {
        if destination.exists() && !force {
            return Err(ConfigError::Exists(destination.to_path_buf()));
        }
        let config = self.load_path(source)?;
        self.save(destination, &config)?;
        Ok(config)
    }

    pub fn schema_json(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(&schema_for!(C)).map_err(ConfigError::Schema)
    }

    pub fn schema_yaml(&self) -> Result<String, ConfigError> {
        serde_yaml::to_string(&schema_for!(C)).map_err(ConfigError::Encode)
    }

    pub fn export_yaml(&self, explicit: Option<&Path>) -> Result<String, ConfigError> {
        let loaded = self.load(explicit)?;
        serde_yaml::to_string(&loaded.config).map_err(ConfigError::Encode)
    }

    pub fn status(&self, explicit: Option<&Path>) -> Result<ConfigStatus, ConfigError> {
        let loaded = self.load(explicit)?;
        Ok(ConfigStatus {
            app: self.spec.app_name.clone(),
            path: loaded.path.display().to_string(),
            exists: loaded.existed,
            valid: true,
            env_var: self.spec.env_var.clone(),
        })
    }

    pub fn execute(
        &self,
        explicit: Option<&Path>,
        command: ConfigCommand,
    ) -> Result<ConfigCommandOutput, ConfigError> {
        let destination = self.resolve_path(explicit);
        match command {
            ConfigCommand::Show => {
                let loaded = self.load(Some(&destination))?;
                let yaml = serde_yaml::to_string(&loaded.config).map_err(ConfigError::Encode)?;
                Ok(ConfigCommandOutput::new(
                    yaml.clone(),
                    serde_json::to_value(&loaded.config).map_err(ConfigError::Schema)?,
                ))
            }
            ConfigCommand::Path => {
                let path = destination.display().to_string();
                Ok(ConfigCommandOutput::new(
                    format!("{path}\n"),
                    serde_json::json!({"path": path}),
                ))
            }
            ConfigCommand::Status => {
                let status = self.status(Some(&destination))?;
                let human = format!(
                    "config: {}\nexists: {}\nvalid: true\n",
                    status.path, status.exists
                );
                Ok(ConfigCommandOutput::new(
                    human,
                    serde_json::to_value(status).map_err(ConfigError::Schema)?,
                ))
            }
            ConfigCommand::Init { force } => {
                self.init(&destination, force)?;
                Ok(ConfigCommandOutput::new(
                    format!("initialized {}\n", destination.display()),
                    serde_json::json!({"initialized": true, "path": destination}),
                ))
            }
            ConfigCommand::Validate { file } => {
                let path = file.unwrap_or(destination);
                self.load_path(&path)?;
                Ok(ConfigCommandOutput::new(
                    format!("valid: {}\n", path.display()),
                    serde_json::json!({"valid": true, "path": path}),
                ))
            }
            ConfigCommand::Schema { format } => {
                let body = match format {
                    SchemaFormat::Json => self.schema_json()?,
                    SchemaFormat::Yaml => self.schema_yaml()?,
                };
                let value = serde_json::to_value(schema_for!(C)).map_err(ConfigError::Schema)?;
                Ok(ConfigCommandOutput::new(format!("{body}\n"), value))
            }
            ConfigCommand::Export { output } => {
                let body = self.export_yaml(Some(&destination))?;
                if let Some(path) = output {
                    atomic_write(&path, body.as_bytes())?;
                    Ok(ConfigCommandOutput::new(
                        format!("exported {}\n", path.display()),
                        serde_json::json!({"exported": true, "path": path}),
                    ))
                } else {
                    let value: serde_json::Value =
                        serde_yaml::from_str(&body).map_err(|source| ConfigError::Parse {
                            app: self.spec.app_name.clone(),
                            source,
                        })?;
                    Ok(ConfigCommandOutput::new(body, value))
                }
            }
            ConfigCommand::Import { file, force } => {
                self.import(&file, &destination, force)?;
                Ok(ConfigCommandOutput::new(
                    format!("imported {} -> {}\n", file.display(), destination.display()),
                    serde_json::json!({"imported": true, "source": file, "path": destination}),
                ))
            }
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let write_target = resolve_write_target(path)?;
    let parent = write_target
        .parent()
        .ok_or_else(|| ConfigError::NoParent(write_target.clone()))?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = write_target.with_extension(format!("yaml.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|source| ConfigError::Write {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| ConfigError::Write {
            path: temporary.clone(),
            source,
        })?;
    fs::rename(&temporary, &write_target).map_err(|source| ConfigError::Write {
        path: write_target,
        source,
    })?;
    Ok(())
}

/// Resolve only the final file's symlink chain. Parent-directory symlinks are
/// intentionally left to the operating system; the temporary file is still
/// created in the target's effective directory and the rename stays atomic.
fn resolve_write_target(path: &Path) -> Result<PathBuf, ConfigError> {
    const MAX_SYMLINKS: usize = 40;

    let mut current = absolute_path(path)?;
    let mut seen = HashSet::new();
    for _ in 0..MAX_SYMLINKS {
        if !seen.insert(symlink_identity(&current)) {
            return Err(ConfigError::SymlinkLoop(path.to_path_buf()));
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let link =
                    fs::read_link(&current).map_err(|source| ConfigError::ResolveSymlink {
                        path: current.clone(),
                        source,
                    })?;
                current = if link.is_absolute() {
                    link
                } else {
                    let parent = current
                        .parent()
                        .ok_or_else(|| ConfigError::NoParent(current.clone()))?;
                    // Do not lexically collapse `..`: if `parent` itself is a
                    // symlink, the OS applies `..` after resolving that parent.
                    parent.join(link)
                };
            }
            Ok(_) => return Ok(current),
            // A dangling final symlink is valid: saving creates its target.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(source) => {
                return Err(ConfigError::ResolveSymlink {
                    path: current,
                    source,
                });
            }
        }
    }
    Err(ConfigError::SymlinkLoop(path.to_path_buf()))
}

fn absolute_path(path: &Path) -> Result<PathBuf, ConfigError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current_dir = std::env::current_dir().map_err(|source| ConfigError::ResolveSymlink {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(current_dir.join(path))
}

fn symlink_identity(path: &Path) -> PathBuf {
    path.parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| lexical_normalize(path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SchemaFormat {
    #[default]
    Json,
    Yaml,
}

/// Drop-in clap subcommand for `app config ...`.
#[derive(Clone, Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved configuration (default values included).
    Show,
    /// Print the resolved configuration path.
    Path,
    /// Report path/existence/validation state.
    Status,
    /// Create a default configuration.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Parse and semantically validate a configuration.
    Validate {
        /// Validate this file instead of the resolved application config.
        file: Option<PathBuf>,
    },
    /// Emit the JSON Schema generated from the host's config type.
    Schema {
        #[arg(long, value_enum, default_value = "json")]
        format: SchemaFormat,
    },
    /// Export the fully resolved/defaulted configuration as YAML.
    Export {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Validate and atomically import a YAML/JSON configuration.
    Import {
        file: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ConfigStatus {
    pub app: String,
    pub path: String,
    pub exists: bool,
    pub valid: bool,
    pub env_var: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigCommandOutput {
    pub human: String,
    pub json: serde_json::Value,
}

impl ConfigCommandOutput {
    #[must_use]
    pub fn new(human: String, json: serde_json::Value) -> Self {
        Self { human, json }
    }

    pub fn print(&self, json: bool) -> Result<(), ConfigError> {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&self.json).map_err(ConfigError::Schema)?
            );
        } else {
            print!("{}", self.human);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {app} configuration: {source}")]
    Parse {
        app: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("configuration validation failed: {0}")]
    Validation(String),
    #[error("serialize configuration: {0}")]
    Encode(serde_yaml::Error),
    #[error("serialize configuration schema: {0}")]
    Schema(serde_json::Error),
    #[error("write configuration {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("resolve configuration symlink {path}: {source}")]
    ResolveSymlink {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("configuration symlink loop while resolving {0}")]
    SymlinkLoop(PathBuf),
    #[error("configuration path has no parent: {0}")]
    NoParent(PathBuf),
    #[error("configuration already exists: {0} (pass --force to replace)")]
    Exists(PathBuf),
    #[error("confirmation required for configuration mutation")]
    ConfirmationRequired,
}

impl ConfigError {
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }
}

impl StructuredError for ConfigError {
    fn category(&self) -> ErrorCategory {
        match self {
            Self::Parse { .. } | Self::Validation(_) | Self::ConfirmationRequired => {
                ErrorCategory::Validation
            }
            Self::Exists(_) => ErrorCategory::ConfigError,
            Self::Read { .. }
            | Self::Encode(_)
            | Self::Schema(_)
            | Self::Write { .. }
            | Self::ResolveSymlink { .. }
            | Self::SymlinkLoop(_)
            | Self::NoParent(_) => ErrorCategory::ExecutionFailure,
        }
    }

    fn code(&self) -> String {
        match self {
            Self::Read { .. } => "config_read",
            Self::Parse { .. } => "config_parse",
            Self::Validation(_) => "config_validation",
            Self::Encode(_) => "config_encode",
            Self::Schema(_) => "config_schema",
            Self::Write { .. } | Self::NoParent(_) => "config_write",
            Self::ResolveSymlink { .. } | Self::SymlinkLoop(_) => "config_symlink",
            Self::Exists(_) => "config_exists",
            Self::ConfirmationRequired => "config_confirmation_required",
        }
        .into()
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct ConfigPathInput {
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct ConfigInitInput {
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub confirmed: bool,
}

/// Register the canonical config tools on an existing mcp-cli router.
///
/// The resolver runs per request, allowing a host context to select an account
/// or profile-specific manager without global mutable state.
pub fn register_config_tools<Ctx, C, F>(router: &mut ToolRouter<Ctx>, resolve: F)
where
    C: AppConfig,
    Ctx: Send + Sync + 'static,
    F: Fn(&Ctx) -> ConfigManager<C> + Send + Sync + Clone + 'static,
{
    let status_resolve = resolve.clone();
    router.add_typed_tool(
        "config_status",
        "Report the resolved config path, existence, and validation state.",
        move |ctx: &Ctx, input: ConfigPathInput| status_resolve(ctx).status(input.path.as_deref()),
    );

    let validate_resolve = resolve.clone();
    router.add_typed_tool(
        "config_validate",
        "Parse and semantically validate the selected config without mutation.",
        move |ctx: &Ctx, input: ConfigPathInput| {
            let manager = validate_resolve(ctx);
            let loaded = manager.load(input.path.as_deref())?;
            Ok::<_, ConfigError>(ConfigStatus {
                app: manager.spec().app_name.clone(),
                path: loaded.path.display().to_string(),
                exists: loaded.existed,
                valid: true,
                env_var: manager.spec().env_var.clone(),
            })
        },
    );

    let schema_resolve = resolve.clone();
    router.add_typed_tool(
        "config_schema",
        "Return the host application's JSON Schema for config.yaml.",
        move |ctx: &Ctx, _input: ConfigPathInput| {
            let schema = schema_resolve(ctx).schema_json()?;
            serde_json::from_str::<serde_json::Value>(&schema).map_err(ConfigError::Schema)
        },
    );

    router.add_typed_tool(
        "config_init",
        "Create a default owner-only config file. Requires confirmed=true.",
        move |ctx: &Ctx, input: ConfigInitInput| {
            if !input.confirmed {
                return Err(ConfigError::ConfirmationRequired);
            }
            let manager = resolve(ctx);
            let path = manager.resolve_path(input.path.as_deref());
            manager.init(&path, input.force)?;
            Ok::<_, ConfigError>(serde_json::json!({"initialized": true, "path": path}))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    struct TestConfig {
        enabled: bool,
        port: u16,
    }

    impl AppConfig for TestConfig {
        fn validate(&self) -> Result<(), ConfigError> {
            if self.port == 1 {
                return Err(ConfigError::validation("port 1 is reserved"));
            }
            Ok(())
        }
    }

    fn manager() -> ConfigManager<TestConfig> {
        ConfigManager::new(ConfigSpec::new("configurable-cli-test"))
    }

    #[test]
    fn absent_empty_and_partial_configs_apply_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let loaded = manager().load(Some(&path)).unwrap();
        assert!(!loaded.existed);
        assert_eq!(loaded.config, TestConfig::default());
        fs::write(&path, "\n").unwrap();
        assert_eq!(
            manager().load(Some(&path)).unwrap().config,
            TestConfig::default()
        );
        fs::write(&path, "enabled: true\n").unwrap();
        assert!(manager().load(Some(&path)).unwrap().config.enabled);
    }

    #[test]
    fn validation_precedes_atomic_import() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.yaml");
        let destination = dir.path().join("config.yaml");
        fs::write(&source, "port: 1\n").unwrap();
        assert!(manager().import(&source, &destination, true).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn owner_only_round_trip_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let config = TestConfig {
            enabled: true,
            port: 42,
        };
        manager().save(&path, &config).unwrap();
        assert_eq!(manager().load(Some(&path)).unwrap().config, config);
        let schema = manager().schema_json().unwrap();
        assert!(schema.contains("enabled"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_follows_relative_config_symlink_without_replacing_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("tracked");
        let target = tracked.join("config.yaml");
        let link = dir.path().join("config.yaml");
        fs::create_dir_all(&tracked).unwrap();
        fs::write(&target, "enabled: false\nport: 9\n").unwrap();
        symlink("tracked/config.yaml", &link).unwrap();
        let link_target = fs::read_link(&link).unwrap();

        let config = TestConfig {
            enabled: true,
            port: 42,
        };
        manager().save(&link, &config).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), link_target);
        assert_eq!(manager().load_path(&target).unwrap(), config);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_follows_multi_hop_and_dangling_symlink_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let links = dir.path().join("links");
        let target = dir.path().join("tracked/config.yaml");
        let first = dir.path().join("config.yaml");
        let second = links.join("config-link");
        fs::create_dir_all(&links).unwrap();
        symlink("links/config-link", &first).unwrap();
        symlink("../tracked/config.yaml", &second).unwrap();

        let config = TestConfig {
            enabled: true,
            port: 43,
        };
        manager().save(&first, &config).unwrap();

        assert!(
            fs::symlink_metadata(&first)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(&second)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(manager().load_path(&target).unwrap(), config);
    }

    #[cfg(unix)]
    #[test]
    fn relative_target_parent_traversal_respects_a_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let physical = dir.path().join("physical");
        let nested = physical.join("nested");
        let expected_target = physical.join("tracked/config.yaml");
        let visible_parent = dir.path().join("visible");
        fs::create_dir_all(&nested).unwrap();
        symlink("physical/nested", &visible_parent).unwrap();
        symlink("../tracked/config.yaml", nested.join("config-link")).unwrap();

        let config = TestConfig {
            enabled: true,
            port: 44,
        };
        manager()
            .save(&visible_parent.join("config-link"), &config)
            .unwrap();

        assert_eq!(manager().load_path(&expected_target).unwrap(), config);
        assert!(!dir.path().join("tracked/config.yaml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_rejects_a_symlink_loop_without_mutation() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("config.yaml");
        let second = dir.path().join("other.yaml");
        symlink("other.yaml", &first).unwrap();
        symlink("config.yaml", &second).unwrap();

        let error = manager()
            .save(
                &first,
                &TestConfig {
                    enabled: true,
                    port: 44,
                },
            )
            .unwrap_err();
        assert!(matches!(error, ConfigError::SymlinkLoop(path) if path == first));
        assert_eq!(fs::read_link(&first).unwrap(), PathBuf::from("other.yaml"));
        assert_eq!(
            fs::read_link(&second).unwrap(),
            PathBuf::from("config.yaml")
        );
    }

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        command: ConfigCommand,
    }

    #[test]
    fn clap_mixin_accepts_expected_commands() {
        for args in [
            vec!["test", "show"],
            vec!["test", "validate"],
            vec!["test", "schema", "--format", "yaml"],
            vec!["test", "init", "--force"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn mcp_mixin_registers_canonical_tools() {
        let mut router: ToolRouter<()> = ToolRouter::new();
        register_config_tools(&mut router, |(): &()| manager());
        let names = router
            .tool_metadata()
            .into_iter()
            .map(|metadata| metadata.name)
            .collect::<Vec<_>>();
        for expected in [
            "config_status",
            "config_validate",
            "config_schema",
            "config_init",
        ] {
            assert!(names.iter().any(|name| name == expected));
        }
    }
}
