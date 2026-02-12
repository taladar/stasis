{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    let
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      inherit (cargoToml.package) name;
      version = "${cargoToml.package.version}-${self.shortRev or self.dirtyShortRev or "unknown"}";
    in
    flake-parts.lib.mkFlake { inherit inputs; } (
      { withSystem, ... }:
      {
        systems = [
          "x86_64-linux"
          "aarch64-linux"
        ];

        perSystem =
          { pkgs, ... }:
          {
            devShells = {
              default = pkgs.mkShell {
                name = "stasis-devshell";
                nativeBuildInputs = with pkgs; [
                  rustc
                  cargo
                  git
                  pkg-config
                  wayland
                  wayland-protocols
                  dbus
                ];
                RUSTFLAGS = "-C target-cpu=native";
                shellHook = ''
                  echo "Entering stasis dev shell — run: cargo build, cargo run, or nix build .#stasis"
                '';
              };
            };

            packages = rec {
              default = pkgs.callPackage ./nix { inherit version name; };
              stasis = default;
            };
          };

        flake = {
          nixosModules = rec {
            stasis = default;

            default =
              { pkgs, ... }:
              let
                module = import ./nix/module.nix {
                  package = withSystem pkgs.stdenv.hostPlatform.system ({ config, ... }: config.packages.default);
                };
              in
              {
                imports = [ module ];
              };
          };

          homeModules = rec {
            stasis = default;

            default =
              { pkgs, ... }:
              let
                module = import ./nix/hm-module.nix {
                  package = withSystem pkgs.stdenv.hostPlatform.system ({ config, ... }: config.packages.default);
                };
              in
              {
                imports = [ module ];
              };
          };

          overlays.default =
            final: prev:
            let
              packages = withSystem prev.stdenv.hostPlatform.system ({ config, ... }: config.packages);
            in
            {
              ${name} = packages.default;
            };
        };
      }
    );
}
