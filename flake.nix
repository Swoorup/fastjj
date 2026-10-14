{
  description = "Prebuilt fastjj binaries for Linux and macOS";

  # 26.05 supports Intel macOS as well as Apple Silicon and both Linux targets.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      release = builtins.fromJSON (builtins.readFile ./nix/binary-release.json);
      systems = builtins.attrNames release.platforms;
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          fastjj = (pkgsFor system).callPackage ./nix/package.nix { inherit release; };
        in
        {
          inherit fastjj;
          default = fastjj;
        }
      );

      apps = forAllSystems (
        system:
        let
          fastjj = {
            type = "app";
            program = "${self.packages.${system}.fastjj}/bin/fastjj";
            meta.description = "A process-private Jujutsu client";
          };
        in
        {
          inherit fastjj;
          default = fastjj;
        }
      );

      checks = forAllSystems (system: {
        inherit (self.packages.${system}) fastjj;
      });
      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
