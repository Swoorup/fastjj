{
  description = "Prebuilt fjj binaries for Linux and macOS";

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
          fjj = (pkgsFor system).callPackage ./nix/package.nix { inherit release; };
        in
        {
          inherit fjj;
          fjjui = fjj;
          default = fjj;
        }
      );

      apps = forAllSystems (
        system:
        let
          fjj = {
            type = "app";
            program = "${self.packages.${system}.fjj}/bin/fjj";
            meta.description = "An embedded Jujutsu client";
          };
          jjui = {
            type = "app";
            program = "${self.packages.${system}.fjj}/bin/fjjui";
            meta.description = "Jujutsu UI connected to the embedded backend";
          };
        in
        {
          inherit fjj jjui;
          fjjui = jjui;
          default = fjj;
        }
      );

      checks = forAllSystems (system: {
        inherit (self.packages.${system}) fjj;
      });
      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
