{
  description = "A basic Rust devshell for NixOS users developing gtk/libadwaita apps";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    ...
  }: let
    systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    forEachSystem = nixpkgs.lib.genAttrs systems;
  in {
    devShells = forEachSystem (
      system: let
        pkgs = nixpkgs.legacyPackages.${system};
        rustPkgs = rust-overlay.packages.${system};
      in rec {
        default = tensaku;
        tensaku = pkgs.mkShell {
          buildInputs = with pkgs; [
            pkg-config
            libGL
            libepoxy
            gtk4
            wrapGAppsHook4 # this is needed for relm4-icons to properly load after gtk::init()
            libadwaita
            fontconfig

            (rustPkgs.rust.override {
              extensions = ["rust-src"];
            })
          ];

          shellHook = ''
            export GSETTINGS_SCHEMA_DIR=${pkgs.glib.getSchemaPath pkgs.gtk4}
          '';
        };
      }
    );
  };
}
