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
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, nix2container }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable."1.84.0".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
        };

        nix2containerPkgs = nix2container.packages.${system};

        # Filter out build artifacts so rustPlatform.buildRustPackage succeeds
        # even if target/ was accidentally committed or is present locally.
        cleanSrc = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let baseName = baseNameOf (toString path);
            in pkgs.lib.cleanSourceFilter path type
               && baseName != "target";
        };

        fibPackage = pkgs.rustPlatform.buildRustPackage {
          pname = "fibonacci-example";
          version = "0.1.0";
          src = cleanSrc;
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
