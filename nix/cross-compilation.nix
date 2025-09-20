# Cross-compilation configuration for different architectures
{ lib, pkgs, system }:

let
  # Define target architectures we want to support
  supportedSystems = [
    "x86_64-linux"
    "aarch64-linux"
    "x86_64-darwin"
    "aarch64-darwin"
  ];

  # Cross-compilation toolchains for each target
  crossTargets = {
    "x86_64-linux" = {
      rustTarget = "x86_64-unknown-linux-gnu";
      cargoConfig = ''
        [target.x86_64-unknown-linux-gnu]
        linker = "${pkgs.gcc}/bin/gcc"
      '';
    };

    "aarch64-linux" = {
      rustTarget = "aarch64-unknown-linux-gnu";
      cargoConfig = ''
        [target.aarch64-unknown-linux-gnu]
        linker = "${pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc}/bin/aarch64-unknown-linux-gnu-gcc"
      '';
      crossPkgs = pkgs.pkgsCross.aarch64-multiplatform;
    };

    "x86_64-darwin" = {
      rustTarget = "x86_64-apple-darwin";
      cargoConfig = ''
        [target.x86_64-apple-darwin]
        linker = "${pkgs.darwin.cctools}/bin/ld"
      '';
    };

    "aarch64-darwin" = {
      rustTarget = "aarch64-apple-darwin";
      cargoConfig = ''
        [target.aarch64-apple-darwin]
        linker = "${pkgs.darwin.cctools}/bin/ld"
      '';
    };
  };

  # Generate cargo configuration for cross-compilation
  generateCargoConfig = targets:
    lib.concatStringsSep "\n" (lib.mapAttrsToList (name: config: config.cargoConfig) targets);

  # Cross-compilation wrapper script
  crossCompileScript = target: pkgs.writeShellScriptBin "cross-compile-${target}" ''
    set -e

    TARGET="${crossTargets.${target}.rustTarget}"
    echo "🔨 Cross-compiling for $TARGET..."

    # Add target if not already installed
    rustup target add $TARGET || true

    # Build for target
    cargo build --release --target $TARGET "$@"

    echo "✅ Cross-compilation complete for $TARGET"
    echo "📦 Binary location: target/$TARGET/release/"
  '';

  # Multi-architecture Docker buildx configuration
  dockerBuildxConfig = pkgs.writeTextFile {
    name = "docker-buildx-config";
    text = ''
      # Docker buildx configuration for multi-architecture builds
      # Usage: docker buildx build --platform linux/amd64,linux/arm64 .

      [buildx]
      default = "multiarch"

      [buildx.multiarch]
      driver = "docker-container"
      platforms = ["linux/amd64", "linux/arm64"]

      [buildx.multiarch.driver-opts]
      network = "host"
    '';
  };

  # Emulation setup for non-native architectures
  emulationSetup = pkgs.writeShellScriptBin "setup-emulation" ''
    echo "🔧 Setting up emulation for cross-compilation..."

    # Register QEMU interpreters for different architectures
    if command -v docker &> /dev/null; then
      docker run --rm --privileged multiarch/qemu-user-static --reset -p yes
      echo "✅ QEMU emulation setup complete"
    else
      echo "⚠️  Docker not found, skipping QEMU setup"
    fi

    # Install binfmt support if on NixOS
    if command -v nixos-rebuild &> /dev/null; then
      echo "🔧 Consider adding to your NixOS configuration:"
      echo "  boot.binfmt.emulatedSystems = [ \"aarch64-linux\" \"armv7l-linux\" ];"
    fi
  '';

in {
  inherit supportedSystems crossTargets;

  # Generate cargo config with all cross-compilation targets
  cargoConfig = generateCargoConfig crossTargets;

  # Cross-compilation scripts for each target
  crossCompileScripts = lib.mapAttrs (name: _: crossCompileScript name) crossTargets;

  # Utilities
  inherit dockerBuildxConfig emulationSetup;

  # Check if a system is supported
  isSupported = system: lib.elem system supportedSystems;

  # Get rust target for a system
  getRustTarget = system:
    if crossTargets ? ${system}
    then crossTargets.${system}.rustTarget
    else throw "Unsupported system: ${system}";
}