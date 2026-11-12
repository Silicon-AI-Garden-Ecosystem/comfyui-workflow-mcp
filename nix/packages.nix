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
      // lib.filterAttrs (_: v: v != null) {
        inherit (pkgs)
          comfyui-workflow-mcp
          ;
      };
    };
}
