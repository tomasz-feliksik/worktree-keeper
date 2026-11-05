{inputs, ...}: {
  imports = [
    inputs.home-manager.flakeModules.home-manager
    inputs.treefmt-nix.flakeModule
  ];
  systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
}
