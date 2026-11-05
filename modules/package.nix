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
    commonArgs = {
      inherit src;
      strictDeps = true;
    };
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;
  in {
    packages.default = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
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
