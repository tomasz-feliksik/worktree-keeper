{inputs, ...}: {
  perSystem = {
    pkgs,
    self',
    system,
    ...
  }: let
    # `stable` is pinned by flake.lock. Not fromToolchainFile, which fetches its
    # manifest during evaluation: that is import-from-derivation, and a consumer
    # that evaluates with it disabled could not build this package at all.
    toolchain = inputs.fenix.packages.${system}.stable.withComponents [
      "cargo"
      "clippy"
      "rust-analyzer"
      "rust-src"
      "rustc"
      "rustfmt"
    ];
    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolchain;
    src = craneLib.cleanCargoSource ../.;
    # The programs the tool runs. They are on PATH for its own tests, and wrapped
    # into the package because a hook cannot rely on the caller's PATH.
    runtimePrograms = [pkgs.git pkgs.direnv] ++ pkgs.lib.optional pkgs.stdenv.hostPlatform.isDarwin pkgs.lsof;
    commonArgs = {
      inherit src;
      strictDeps = true;
      nativeCheckInputs = runtimePrograms;
    };
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;
  in {
    packages.default = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        nativeBuildInputs = [pkgs.makeWrapper];
        postInstall = ''
          wrapProgram "$out/bin/worktree" --prefix PATH : ${pkgs.lib.makeBinPath runtimePrograms}
        '';
        meta.mainProgram = "worktree";
      });
    checks = {
      package = self'.packages.default;
      clippy = craneLib.cargoClippy (commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- --deny warnings";
        });
      # The black-box suite against the package that ships, in the sandbox. Its
      # scaffolding gets git and direnv here, the tool gets them from its wrapper,
      # and the second half proves that wrapper with nothing else on PATH.
      parity =
        pkgs.runCommand "worktree-keeper-parity" {
          nativeBuildInputs = [pkgs.git pkgs.direnv];
        } ''
          export WORKTREE=${pkgs.lib.getExe self'.packages.default}
          bash ${../tests/parity/test.sh}

          export HOME=$PWD/home GIT_CONFIG_GLOBAL=$PWD/gitconfig GIT_CONFIG_NOSYSTEM=1
          mkdir "$HOME"
          git config --global user.name test
          git config --global user.email test@example.invalid
          git init -q -b main repo
          echo "export WRAPPED=1" >repo/.envrc
          git -C repo add .envrc
          git -C repo commit -q -m init
          git -C repo worktree add -q -b wrapped ../wt
          (cd repo && direnv allow .)
          said=$(env -i HOME="$HOME" GIT_CONFIG_GLOBAL="$GIT_CONFIG_GLOBAL" PATH=/nonexistent \
            "$WORKTREE" provision "$PWD/wt" 2>&1)
          case $said in
          *"worktree .envrc allowed"*) ;;
          *)
            echo "the wrapped binary did not reach git and direnv on its own: $said" >&2
            exit 1
            ;;
          esac
          touch "$out"
        '';
    };
    devShells.default = pkgs.mkShell {
      packages = [toolchain] ++ (with pkgs; [just cargo-deny cargo-audit cargo-mutants git direnv lsof markdownlint-cli2]);
    };
  };
}
