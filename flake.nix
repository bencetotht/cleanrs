{
  description = "APFS-aware developer storage analysis and cleanup";

  # nixos-unstable no longer supports x86_64-darwin; Intel Macs use the
  # Homebrew formula or the prebuilt tarball instead.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      manifest = nixpkgs.lib.importTOML ./Cargo.toml;
      binaryManifest = nixpkgs.lib.importTOML ./cleanrs/Cargo.toml;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib stdenv;
          cleanrs = pkgs.rustPlatform.buildRustPackage {
            pname = "cleanrs";
            version = manifest.workspace.package.version;
            # Explicit build inputs exclude .git, target, docs, and local state.
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./cleanrs
                ./cleanrs-core
                ./README.md
                ./LICENSE
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "cleanrs"
            ];
            cargoTestFlags = [ "--workspace" ];
            nativeBuildInputs = [ pkgs.installShellFiles ];
            # Project-hygiene fixtures initialize and inspect temporary Git repos.
            nativeCheckInputs = [ pkgs.git ];
            # Linux Nix sandboxes supply BusyBox at /bin/sh. This fixture invokes
            # it via a symlink named "provider", which BusyBox rejects as an
            # unknown applet. It passes with a normal host shell; keep all other tests.
            checkFlags = lib.optionals stdenv.hostPlatform.isLinux [
              "--skip"
              "execution::tests::symlinked_executable_entry_is_pinned_with_its_resolved_target"
            ];
            postInstall = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
              installShellCompletion --cmd cleanrs \
                --bash <($out/bin/cleanrs completions bash) \
                --zsh <($out/bin/cleanrs completions zsh) \
                --fish <($out/bin/cleanrs completions fish)
              $out/bin/cleanrs man > cleanrs.1
              installManPage cleanrs.1
            '';
            meta = {
              description = binaryManifest.package.description;
              homepage = manifest.workspace.package.homepage;
              license = lib.licenses.mit;
              mainProgram = "cleanrs";
              platforms = lib.platforms.unix;
            };
          };
        in
        {
          inherit cleanrs;
          default = cleanrs;
        }
      );
      checks = forAllSystems (system: {
        default = self.packages.${system}.default;
      });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          meta = self.packages.${system}.default.meta;
          program = "${self.packages.${system}.default}/bin/cleanrs";
        };
      });
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
              clippy
              rustfmt
              rust-analyzer
              git
            ];
          };
        }
      );
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
