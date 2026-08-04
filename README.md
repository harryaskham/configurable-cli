# configurable-cli

Canonical `~/.config/<app>/config.yaml` support for Rust CLIs in the
`mcp-cli` ecosystem.

A host provides one serde/schemars type and a small `ConfigSpec`.
`configurable-cli` then supplies:

- path precedence: explicit CLI path, `$APP_CONFIG`, `$XDG_CONFIG_HOME`, `$HOME`;
- defaults for absent/empty/partial files;
- semantic validation through `AppConfig::validate`;
- owner-only (`0600`) atomic writes;
- validate-before-write import and fully-defaulted export;
- generated JSON Schema;
- reusable clap `config` subcommands;
- matching `config_status`, `config_validate`, `config_schema`, and
  confirmation-gated `config_init` MCP tools.

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
