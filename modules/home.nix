# homeModules.default: `worktree` on PATH, with the placement root baked in.
#
# The root is baked in with wrapProgram rather than set as a session variable,
# because hooks and agents started outside a login shell never see those. It uses
# --set-default, so an explicit WORKTREE_ROOT in the environment still wins.
{
  self,
  inputs,
  ...
}: let
  module = {
    config,
    lib,
    pkgs,
    ...
  }: let
    cfg = config.worktreeKeeper;
  in {
    key = "worktree-keeper/homeModules/default";

    options.worktreeKeeper = {
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        defaultText = lib.literalExpression "worktree-keeper's own package";
        description = "The `worktree` package to install.";
      };
      root = lib.mkOption {
        type = lib.types.str;
        example = lib.literalExpression ''"''${config.home.homeDirectory}/worktrees"'';
        description = ''
          The placement root: every worktree lives at `<root>/<repo>/<slug>`.
          No default, because a default would silently put worktrees somewhere
          nobody chose.
        '';
      };
      finalPackage = lib.mkOption {
        type = lib.types.package;
        readOnly = true;
        description = ''
          The configured command, with the root baked in. For hooks and other
          modules to reference by store path, since a hook run by a long-lived
          server cannot rely on PATH.
        '';
      };
    };

    config = {
      worktreeKeeper.finalPackage = pkgs.symlinkJoin {
        name = "worktree-configured";
        paths = [cfg.package];
        nativeBuildInputs = [pkgs.makeWrapper];
        postBuild = ''
          wrapProgram "$out/bin/worktree" --set-default WORKTREE_ROOT ${lib.escapeShellArg cfg.root}
        '';
        meta.mainProgram = "worktree";
      };
      home.packages = [cfg.finalPackage];
    };
  };
in {
  flake.homeModules.default = module;

  perSystem = {pkgs, ...}: let
    home = inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home = {
            username = "test";
            homeDirectory = "/home/test";
            stateVersion = "25.11";
          };
          worktreeKeeper.root = "/home/test/worktrees";
        }
      ];
    };
  in {
    # Asserts the behaviour: the root reaches the command through its wrapper.
    # Read from the wrapper, since no subcommand prints the root yet.
    checks.home-module = pkgs.runCommand "home-module-check" {} ''
      wrapper=${home.config.worktreeKeeper.finalPackage}/bin/worktree
      grep -F 'WORKTREE_ROOT' "$wrapper" | grep -F '/home/test/worktrees'
      "$wrapper" --version
      touch "$out"
    '';
  };
}
