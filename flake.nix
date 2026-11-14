{
  description = "Dev shell for flakemill";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }: {
    devShells.x86_64-linux.default = nixpkgs.legacyPackages.x86_64-linux.mkShell {
      packages = with nixpkgs.legacyPackages.x86_64-linux; [
        cargo
        rustc
        rustfmt
        clippy
        git
        nix
        cachix
        minisign
      ];
    };
  };
}
