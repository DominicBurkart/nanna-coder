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

  # Individual workspace member builds for granular container images
  harness = craneLib.buildPackage {
    inherit src cargoArtifacts;
    buildInputs = commonBuildInputs;
    nativeBuildInputs = commonNativeBuildInputs;

    cargoBuildCommand = "cargo build --release --bin harness";
    cargoCheckCommand = "cargo check --bin harness";
    cargoTestCommand = "cargo test --package harness";

    # Install only the harness binary
    installPhase = ''
      mkdir -p $out/bin
      cp target/release/harness $out/bin/
    '';
  };

in
{
  inherit nanna-coder harness;
  inherit cargoArtifacts commonBuildInputs commonNativeBuildInputs;
}
