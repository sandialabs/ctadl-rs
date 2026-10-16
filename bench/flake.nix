{
  description = "Continuous ctadl benchmarking: the runner's toolchain and its pinned inputs";

  # The same nixpkgs revision as the top-level flake (see ../flake.lock), so Ghidra here is the
  # Ghidra ctadl's own regression suite runs against.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/09590de4376b97da77dee8de1e5bcfc2dc5aa347";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (
        pkgs:
        let
          # TaintBench APKs, fetched by the hashes in each app's app.json -- the same fetch as
          # ../nix/taintbench.nix, but as a directory the runner reads from instead of a check.
          appsDir = ../taintbench/apps;
          entries = builtins.readDir appsDir;
          apps = builtins.filter (
            n: entries.${n} == "directory" && builtins.pathExists (appsDir + "/${n}/app.json")
          ) (builtins.attrNames entries);
          meta = n: builtins.fromJSON (builtins.readFile (appsDir + "/${n}/app.json"));
          included = builtins.filter (n: !((meta n) ? excluded)) apps;

          droidbenchRev = "a57fa6f42f278591695672f1aa8b37c275139370";
          droidbench = name: hash: {
            name = "${name}.apk";
            path = pkgs.fetchurl {
              url = "https://raw.githubusercontent.com/secure-software-engineering/DroidBench/${droidbenchRev}/apk/InterComponentCommunication/${name}.apk";
              inherit hash;
            };
          };
        in
        {
          python = pkgs.python3;
          ghidra = pkgs.ghidra-bin;

          taintbench-apks = pkgs.linkFarm "taintbench-apks" (
            builtins.map (n: {
              name = "${n}.apk";
              path = pkgs.fetchurl {
                url = (meta n).apk.url;
                hash = (meta n).apk.sha256;
              };
            }) included
          );

          # Mirrors droidbenchIccApks in ../flake.nix.
          android-icc-apks = pkgs.linkFarm "droidbench-icc-apks" [
            (droidbench "ActivityCommunication2" "sha256-KyJvdF2BdCccHox9k+op/GbzEbeHKGmQ6op/HOrxvcA=")
            (droidbench "ActivityCommunication5" "sha256-MVtam+x1OU2TUN2PBaJnULM2PlC/jBawvLwPdS05FVw=")
            (droidbench "ComponentNotInManifest1" "sha256-MHB8889dYdMwcUE6GJIcpkWTWEExEd/6iUna+ArFrHc=")
          ];

          # Everything the daemon reads at startup, as one store path: `inputs.json` names the
          # tools and data above so the runner never resolves them on its own.
          inputs = pkgs.writeText "ctadl-bench-inputs.json" (
            builtins.toJSON {
              python = "${pkgs.python3}/bin/python3";
              ghidra_home = "${pkgs.ghidra-bin}/lib/ghidra";
              taintbench_apks = "${self.packages.${pkgs.system}.taintbench-apks}";
              android_icc_apks = "${self.packages.${pkgs.system}.android-icc-apks}";
              git = "${pkgs.git}/bin/git";
            }
          );

          default = self.packages.${pkgs.system}.inputs;
        }
      );

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.python3 pkgs.sqlite pkgs.git ];
        };
      });
    };
}
