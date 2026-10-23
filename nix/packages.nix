{
  perSystem =
    {
      lib,
      pkgs,
      self',
      ...
    }:
    {
      packages = {
        default = self'.packages.comfyui-workflow-mcp;
      }
      // builtins.intersectAttrs (lib.genAttrs [
        "comfyui-workflow-mcp"
      ] (_: null)) pkgs;
    };
}
