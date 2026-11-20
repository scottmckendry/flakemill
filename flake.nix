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
          runtimeDeps = [
            pkgs.curl
            pkgs.minisign
            pkgs.git
            pkgs.cachix
          ];
          flakemill = pkgs.rustPlatform.buildRustPackage {
            pname = "flakemill";
            inherit version;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.makeWrapper
              pkgs.installShellFiles
            ];
            postInstall = ''
              installShellCompletion --cmd flakemill \
                --bash <("$out/bin/flakemill" completions bash) \
                --fish <("$out/bin/flakemill" completions fish) \
                --zsh <("$out/bin/flakemill" completions zsh)
              mkdir -p "$out/share/nushell/vendor/autoload"
              "$out/bin/flakemill" completions nushell > "$out/share/nushell/vendor/autoload/flakemill.nu"
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
            ];
          };
        }
      );
    };
}
