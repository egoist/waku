{
  description = "Waku native coding-agent desktop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      forSystem =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          runtimeLibraries = with pkgs; [
            fontconfig
            freetype
            libGL
            libx11
            libxcb
            libxkbcommon
            vulkan-loader
            wayland
          ];
          runtimeTools = with pkgs; [
            git
            curl
            xdg-utils
          ];
          waku = pkgs.rustPlatform.buildRustPackage {
            pname = "waku";
            version = manifest.package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./.cargo
                ./build.rs
                ./src
                ./crates
                ./db/migrations
                ./assets
                ./resources
                ./locales
                ./website/public/app-icon.png
                ./LICENSE
              ];
            };

            cargoHash = "sha256-6RwMQEUwYYgHEgzY1thsUypfyEE8VLxO4B0WvHFbSY0=";

            nativeBuildInputs = with pkgs; [
              cmake
              pkg-config
              rustPlatform.bindgenHook
              makeWrapper
            ];
            buildInputs =
              runtimeLibraries
              ++ (with pkgs; [
                openssl
                zlib
              ]);
            dontUseCmakeConfigure = true;

            cargoBuildFlags = [
              "--package=waku"
              "--bin=waku"
              "--bin=waku-updater"
              "--package=waku-daemon"
              "--bin=waku-daemon"
            ];
            doCheck = false;

            postPatch = ''
              substituteInPlace crates/waku-core/src/usage.rs \
                --replace-fail '"/usr/bin/curl"' '"${lib.getExe pkgs.curl}"'
            '';

            postInstall = ''
              install -Dm644 resources/linux/sh.waku.desktop \
                "$out/share/applications/sh.waku.desktop"
              substituteInPlace "$out/share/applications/sh.waku.desktop" \
                --replace-fail 'Exec=waku' "Exec=$out/bin/waku"
              install -Dm644 website/public/app-icon.png \
                "$out/share/icons/hicolor/256x256/apps/sh.waku.png"
              install -Dm644 LICENSE "$out/share/licenses/waku/LICENSE"
            '';

            postFixup = ''
              patchelf --add-rpath "${lib.makeLibraryPath runtimeLibraries}" "$out/bin/waku"
              for executable in waku waku-daemon; do
                wrapProgram "$out/bin/$executable" \
                  --suffix PATH : "${lib.makeBinPath runtimeTools}"
              done
            '';

            meta = {
              inherit (manifest.package) description;
              homepage = "https://waku.sh";
              license = lib.licenses.gpl3Only;
              platforms = systems;
              mainProgram = "waku";
            };
          };
        in
        {
          inherit waku;
          devShell = pkgs.mkShell {
            inputsFrom = [ waku ];
            packages =
              (with pkgs; [
                cargo
                rustc
                rustfmt
                clippy
                bun
              ])
              ++ runtimeTools;
            LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibraries;
          };
        };
      perSystem = forAllSystems forSystem;
    in
    {
      packages = forAllSystems (system: {
        default = perSystem.${system}.waku;
        waku = perSystem.${system}.waku;
      });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.waku}/bin/waku";
          meta = self.packages.${system}.waku.meta;
        };
      });
      devShells = forAllSystems (system: {
        default = perSystem.${system}.devShell;
      });
    };
}
