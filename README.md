# configurable-cli

Canonical `~/.config/<app>/config.yaml` support for Rust CLIs in the
`mcp-cli` ecosystem.

A host provides one serde/schemars type and a small `ConfigSpec`.
`configurable-cli` then supplies:

- path precedence: explicit CLI path, `$APP_CONFIG`, `$XDG_CONFIG_HOME`, `$HOME`;
- defaults for absent/empty/partial files;
- semantic validation through `AppConfig::validate`;
- owner-only (`0600`) atomic writes that follow relative or multi-hop config
  symlinks and replace the real target without unlinking the symlink;
- validate-before-write import and fully-defaulted export;
- generated JSON Schema;
- reusable clap `config` subcommands;
- matching `config_status`, `config_validate`, `config_schema`, and
  confirmation-gated `config_init` MCP tools.

## JSON and shared files

A file name ending in `.json` (or `ConfigSpec::with_format(ConfigFormat::Json)`)
reads and writes pretty JSON instead of YAML. For a file that another program
also owns, `ConfigSpec::preserving_unknown_keys()` merges each save into the
existing document: top-level keys the host type declares are replaced, or
removed when the host no longer serializes them, and every other key is kept
exactly. A document that does not parse is never overwritten.

`Loaded::revision` is a `sha256:` digest of the bytes that were read.
`ConfigManager::save_if_revision(path, config, revision)` refuses with
`ConfigError::Conflict` if another writer changed the file since then (`None`
means the file must not exist yet).

```rust,ignore
let manager = ConfigManager::<Namespaces>::new(
    ConfigSpec::new("tyler").with_file_name("config.json").preserving_unknown_keys(),
);
let loaded = manager.load(None)?;
manager.save_if_revision(&loaded.path, &edited, loaded.revision.as_deref())?;
```

## Host integration

```rust
use configurable_cli::{AppConfig, ConfigError, ConfigManager, ConfigSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Config { port: u16 }

impl AppConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 { return Err(ConfigError::validation("port must be non-zero")); }
        Ok(())
    }
}

let manager = ConfigManager::<Config>::new(ConfigSpec::new("myapp"));
let loaded = manager.load(None)?;
# Ok::<(), ConfigError>(())
```

Embed [`ConfigCommand`] in the host's clap tree and pass it to
`ConfigManager::execute`. The standard surface is:

```text
myapp config show
myapp config path
myapp config status
myapp config init [--force]
myapp config validate [FILE]
myapp config schema [--format json|yaml]
myapp config export [-o FILE]
myapp config import FILE [--force]
```

The host may treat bare `myapp config` as `show` by storing the nested command
as `Option<ConfigCommand>`.

## MCP mixin

```rust,ignore
configurable_cli::register_config_tools(&mut router, |_ctx| {
    ConfigManager::<Config>::new(ConfigSpec::new("myapp"))
});
```

Mutating tools fail closed unless their typed input carries `confirmed: true`.
No credential or secret values are inspected by this crate.
