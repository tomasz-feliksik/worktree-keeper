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
    runtimePrograms = [pkgs.git pkgs.direnv];
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
    };
    devShells.default = pkgs.mkShell {
      packages = [toolchain] ++ (with pkgs; [just cargo-deny cargo-audit cargo-mutants git direnv lsof markdownlint-cli2]);
    };
  };
}
