{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    bun2nix = {
      url = "github:nix-community/bun2nix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.treefmt-nix.follows = "treefmt-nix";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      ...
    }@inputs:
    with builtins;
    with nixpkgs.lib;

    let
      # bun < 1.4 needs its AVX2 baseline build to run on older CPUs; 1.4+ dropped that
      # requirement, so pin a newer release on x86_64-linux instead of carrying a baseline variant.
      bunOverlay =
        final: prev:
        optionalAttrs (prev.stdenv.hostPlatform.system == "x86_64-linux") {
          bun = prev.bun.overrideAttrs (oldAttrs: rec {
            version = "1.4.1";
            src = prev.fetchurl {
              url = "https://github.com/oven-sh/bun/releases/download/bun-v${version}/bun-linux-x64.zip";
              hash = "sha256-dMHDvufNmYUAyPlpzYlyNVrGoHIH6Uo57s4ZmbVv+r8=";
            };
          });
        };

      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [
            bunOverlay
            inputs.bun2nix.overlays.default
          ];
        };

      # bun2nix doesn't ship a stable `bun.nix` in the repo, so regenerate it from bun.lock
      # at build time with the bun2nix CLI itself.
      bunNixFor =
        pkgs: src:
        pkgs.stdenv.mkDerivation {
          inherit src;
          name = "bun.nix";
          nativeBuildInputs = [ pkgs.bun2nix ];
          buildPhase = "bun2nix -o bun.nix";
          installPhase = "cp bun.nix $out";
        };

      iloaderFor =
        pkgs:
        let
          src = cleanSource ./.;
          json = fromJSON (readFile (src + "/package.json"));
        in
        pkgs.rustPlatform.buildRustPackage (final: {
          pname = json.name;
          inherit (json) version;

          inherit src;
          bunDeps = pkgs.bun2nix.fetchBunDeps {
            bunNix = bunNixFor pkgs src;
          };
          cargoRoot = "src-tauri";
          cargoLock = {
            lockFile = src + "/${final.cargoRoot}/Cargo.lock";
            # apple-codesign and isideload are pulled from git rather than crates.io.
            outputHashes = {
              "apple-codesign-0.1.0" = "sha256-1ajD3aHa6mUuMYVH8jluIh49J0vKTp4vrfX4T2i3oTg=";
              "isideload-0.3.17" = "sha256-oGE+dY68Gv1rmTSHCycakkSCMvUN9YjZHB1gJSikuho=";
            };
          };
          buildAndTestSubdir = final.cargoRoot;

          dontUseBunBuild = true;
          dontUseBunCheck = true;

          nativeBuildInputs =
            with pkgs;
            [
              cargo-tauri.hook
              bun2nix.hook
              pkg-config
              jq
              moreutils
            ]
            ++ optionals stdenv.hostPlatform.isLinux [ wrapGAppsHook4 ];

          buildInputs = optionals pkgs.stdenv.hostPlatform.isLinux (
            with pkgs;
            [
              glib-networking
              openssl
              webkitgtk_4_1
            ]
          );

          postPatch = ''
            jq \
              '.plugins.updater.endpoints = [ ]
              | .bundle.createUpdaterArtifacts = false' \
              ${final.cargoRoot}/tauri.conf.json \
              | sponge ${final.cargoRoot}/tauri.conf.json
          '';
        });
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = pkgsFor system;
        iloader = iloaderFor pkgs;
      in
      {
        packages = {
          inherit iloader;
          default = iloader;
        };

        formatter = inputs.treefmt-nix.lib.mkWrapper pkgs {
          programs.nixfmt.enable = true;
        };
      }
    )
    // {
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.iloader;
        in
        {
          options.programs.iloader = {
            enable = lib.mkEnableOption "iloader, a user friendly iOS sideloader";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
              description = "The iloader package to use.";
            };
          };

          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
            # iloader talks to devices over usbmux, same as libimobiledevice tools.
            services.usbmuxd.enable = true;
          };
        };
    };
}
