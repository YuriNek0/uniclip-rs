{
  description = "Cross-platform shared clipboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      allSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs allSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          uniclip = pkgs.rustPlatform.buildRustPackage (finalAttrs: {
            name = "uniclip-rs";
            pname = "uniclip-rs";
            version = "0.0.1";
            src = ./.;
            cargoHash = "sha256-/j7CAG559d6luHjwHCI1fOn0dqdScO7KoNmppU73cGM=";

            meta = with pkgs.lib; {
              description = "A rust fork of uniclip - Cross-platform shared clipboard";
              homepage = "https://github.com/yurinek0/uniclip-rs";
              license = licenses.mit;
              platforms = [ pkgs.stdenv.hostPlatform.system ];
              mainProgram = "uniclip-rs";
            };
          });

          default = self.packages.${system}.uniclip;
        }
      );

      defaultPackage = forAllSystems (system: self.packages.${system}.default);
    };
}
