{
  config,
  ...
}:
{
  flake.homeModules = {
    default = config.flake.homeModules.comfyui-workflow-mcp;

    comfyui-workflow-mcp = ./_homeModules/comfyui-workflow-mcp;
  };
}
