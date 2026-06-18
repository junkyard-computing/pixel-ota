{
  description = "pixel-ota: online A/B updates for a Pixel running Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # Cross to aarch64 + musl => static, portable binary for the Pixel's Debian.
      cross = import nixpkgs {
        inherit system;
        crossSystem = { config = "aarch64-unknown-linux-musl"; };
      };
    in {
      packages.${system} = {
        default = cross.rustPlatform.buildRustPackage {
          pname = "pixel-ota";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          RUSTFLAGS = "-C target-feature=+crt-static";
          doCheck = false;
        };
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy ];
      };
    };
}
