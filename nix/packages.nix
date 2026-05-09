# Package definitions for nanna-coder workspace
# This module contains:
# - Core package builds (nanna-coder, harness)
# - Common build inputs and configurations
# - Source filtering logic

{ pkgs
, lib
, craneLib
, src
}:

let
  # Common build inputs for all Rust packages
  commonBuildInputs = with pkgs; [
    pkg-config
    openssl
    libssh2
    zlib
  ];

  commonNativeBuildInputs = with pkgs; [
    pkg-config
    stdenv.cc
  ];

  # Build dependencies first for better caching
  cargoArtifacts = craneLib.buildDepsOnly {
    inherit src;
    buildInputs = commonBuildInputs;
    nativeBuildInputs = commonNativeBuildInputs;
  };

  # Build the workspace
  nanna-coder = craneLib.buildPackage {
    inherit src cargoArtifacts;
    buildInputs = commonBuildInputs;
    nativeBuildInputs = commonNativeBuildInputs;

    # Ensure all workspace members are built
    cargoBuildCommand = "cargo build --workspace --release";
    cargoCheckCommand = "cargo check --workspace";
    cargoTestCommand = "cargo test --workspace";

    # Additional build metadata
    meta = with lib; {
      description = "AI-powered coding assistant with tool calling and multi-model support";
      homepage = "https://github.com/yourusername/nanna-coder";
      license = licenses.mit;
      maintainers = [ ];
      platforms = platforms.linux ++ platforms.darwin;
    };
  };

  # Individual workspace member build for granular container images.
  # Crate name is `harness`, binary is `nanna` (renamed from `harness`).
  nanna = craneLib.buildPackage {
    inherit src cargoArtifacts;
    buildInputs = commonBuildInputs;
    nativeBuildInputs = commonNativeBuildInputs;

    cargoBuildCommand = "cargo build --release --bin nanna";
    cargoCheckCommand = "cargo check --bin nanna";
    cargoTestCommand = "cargo test --package harness";

    # Install only the nanna binary
    installPhase = ''
      mkdir -p $out/bin
      cp target/release/nanna $out/bin/
    '';
  };

  # Backwards-compat alias so anyone calling `nix build .#harness` still
  # gets a working binary during the rename's grace period.
  harness = nanna;

in
{
  inherit nanna-coder nanna harness;
  inherit cargoArtifacts commonBuildInputs commonNativeBuildInputs;
}
