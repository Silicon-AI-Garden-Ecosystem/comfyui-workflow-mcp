{
  config,
  inputs,
  ...
}:
{
  flake.overlays = {
    default = import ./_overlays/default.nix;
  };

  perSystem =
    {
      system,
      ...
    }:
    {
      _module.args.pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [
          config.flake.overlays.default
        ];
      };
    };
}
