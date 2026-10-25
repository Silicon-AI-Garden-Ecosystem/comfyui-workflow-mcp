{
  config,
  ...
}:
{
  flake.homeModules = {
    default = config.flake.homeModules.comfyui-workflow-mcp;

    comfyui-workflow-mcp = import ./_homeModules/comfyui-workflow-mcp;
  };
}
