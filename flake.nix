{
  description = "configurable-cli — canonical XDG YAML configuration, validation, schema, clap, and MCP mixin";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    mcp-cli = {
      url = "git+https://github.com/harryaskham/mcp-cli?ref=main";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, mcp-cli, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        cargoConfig = pkgs.writeText "configurable-cli-cargo-config.toml" ''
          [patch."https://github.com/harryaskham/mcp-cli"]
          mcp-cli-core = { path = "${mcp-cli}" }
        '';
        configurableCli = pkgs.rustPlatform.buildRustPackage {
          pname = "configurable-cli";
          version = "0.1.0";
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };
          postPatch = ''
            mkdir -p .cargo
            cat ${cargoConfig} >> .cargo/config.toml
          '';
          doCheck = true;
          meta = {
            description = "Canonical XDG YAML configuration, validation, schema, clap, and MCP mixin";
            homepage = "https://github.com/harryaskham/configurable-cli";
            license = pkgs.lib.licenses.mit;
          };
        };
      in
      {
        packages.default = configurableCli;
        packages.configurable-cli = configurableCli;
        checks.test = configurableCli;
        apps.release = {
          type = "app";
          program = "${pkgs.writeShellScript "configurable-cli-release" ''
            export PATH="${pkgs.lib.makeBinPath [ pkgs.git pkgs.cargo ]}:$PATH"
            exec ${pkgs.bash}/bin/bash ${./scripts/release.sh} "$@"
          ''}";
        };
        devShells.default = pkgs.mkShell {
          inputsFrom = [ configurableCli ];
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
        };
        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
