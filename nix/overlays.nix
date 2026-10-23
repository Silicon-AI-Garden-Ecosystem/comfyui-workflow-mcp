{
  config,
  ...
}:
{
  flake.overlays = {
    default = import ./_overlays/default.nix;
  };

  perSystem =
    {
      inputs',
      ...
    }:
    {
      _module.args.pkgs = inputs'.nixpkgs.legacyPackages.extend config.flake.overlays.default;
    };
}
