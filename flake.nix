{
  description = "flakemill — external flake update service and client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

      mkPackages =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          flakemill = pkgs.rustPlatform.buildRustPackage {
            pname = "flakemill";
            inherit version;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram "$out/bin/flakemill" \
                --prefix PATH : "${pkgs.lib.makeBinPath [ pkgs.curl pkgs.minisign ]}"
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
          packages = mkPackages system;
        in
        {
          default = {
            type = "app";
            program = "${packages.flakemill}/bin/flakemill";
            meta.description = "Run the flakemill update service";
          };
          flakemill = {
            type = "app";
            program = "${packages.flakemill}/bin/flakemill";
            meta.description = "Run the flakemill update service";
          };
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
            ];
          };
        }
      );
    };
}
