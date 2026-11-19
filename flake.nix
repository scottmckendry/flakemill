{
  description = "flakemill — external flake update service and client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      cargoVersion = (fromTOML (builtins.readFile ./Cargo.toml)).package.version;
      version = "${cargoVersion}+${self.shortRev or (self.dirtyShortRev or "dirty")}";

      mkPackages =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # Commands the binary shells out to. `fetch`/`apply` need curl and
          # minisign; `serve` additionally runs the update pipeline (git, nix,
          # cachix) and spawns nix-serve for the local binary cache. Wrapping
          # them in PATH keeps `nix run` working on a host without them.
          runtimeDeps = [
            pkgs.curl
            pkgs.minisign
            pkgs.git
            pkgs.nix # provides nix-store as well
            pkgs.cachix
            pkgs.nix-serve-ng # provides nix-serve
          ];
          flakemill = pkgs.rustPlatform.buildRustPackage {
            pname = "flakemill";
            inherit version;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram "$out/bin/flakemill" \
                --prefix PATH : "${pkgs.lib.makeBinPath runtimeDeps}"
            '';
            meta = {
              description = "External update service for Nix flakes";
              homepage = "https://github.com/scottmckendry/flakemill";
              license = pkgs.lib.licenses.mit;
              platforms = [ "x86_64-linux" ];
              mainProgram = "flakemill";
            };
          };

        in
        {
          inherit flakemill;
          default = flakemill;
        };
    in
    {
      packages = forAllSystems mkPackages;

      apps = forAllSystems (
        system:
        let
          app = {
            type = "app";
            program = "${self.packages.${system}.flakemill}/bin/flakemill";
            meta.description = "Run the flakemill update service";
          };
        in
        {
          default = app;
          flakemill = app;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              git
              nix
              cachix
              minisign
              curl
              # so `cargo test` exercises the real local cache paths (the tests
              # that spawn nix-serve skip themselves when it is absent)
              nix-serve-ng
            ];
          };
        }
      );
    };
}
