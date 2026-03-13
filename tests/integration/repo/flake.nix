{
  description = "Fibonacci example project for nanna-coder dev container testing";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nanna-coder = {
      url = "path:../../..";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-overlay.follows = "rust-overlay";
      inputs.nix2container.follows = "nix2container";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, nix2container, nanna-coder }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable."1.84.0".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
        };

        nix2containerPkgs = nix2container.packages.${system};

        fibPackage = pkgs.rustPlatform.buildRustPackage {
          pname = "fibonacci-example";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        devContainerPackages = [
          rustToolchain
          pkgs.cargo-nextest
          pkgs.bash
          pkgs.coreutils
          pkgs.git
          pkgs.cacert
          pkgs.pkg-config
          pkgs.openssl
        ];

        devContainerImage = nix2containerPkgs.nix2container.buildImage {
          name = "fibonacci-example-dev";
          tag = "latest";

          copyToRoot = pkgs.buildEnv {
            name = "dev-env";
            paths = devContainerPackages;
            pathsToLink = [ "/bin" "/etc" "/share" "/lib" "/include" ];
          };

          config = {
            Cmd = [ "sleep" "infinity" ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "PATH=/bin"
              "RUST_LOG=info"
            ];
            WorkingDir = "/workspace";
          };

          maxLayers = 100;
        };

      in
      {
        packages = {
          default = fibPackage;
          inherit devContainerImage;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = devContainerPackages;
        };
      }
    );
}
