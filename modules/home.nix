# homeModules.default: `worktree` on PATH. It has nothing to configure: worktrees go
# beside the repository they belong to.
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
    };

    config.home.packages = [cfg.package];
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
        }
      ];
    };
  in {
    # Asserts the behaviour: the module puts a working command on the home's PATH.
    checks.home-module = pkgs.runCommand "home-module-check" {} ''
      ${home.config.home.path}/bin/worktree --version
      touch "$out"
    '';
  };
}
