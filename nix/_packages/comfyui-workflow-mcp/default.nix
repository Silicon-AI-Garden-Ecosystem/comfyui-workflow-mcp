{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "comfyui-workflow-mcp";
  version = "0.4.0";

  src = ../../..;

  cargoHash = "sha256-JlfjMV8sP0wjJ80ijFWwesLmZoDvOINyyhnsZFFPtoI=";

  doCheck = true;

  meta = {
    description = "A ComfyUI workflow MCP friendly to both humans and agents";
    homepage = "https://github.com/Silicon-AI-Garden-Ecosystem/comfyui-workflow-mcp";
    changelog = "https://github.com/Silicon-AI-Garden-Ecosystem/comfyui-workflow-mcp/releases/tag/v${finalAttrs.version}";
    license = lib.licenses.mit;
    maintainers = with lib.maintainers; [ knightfemale ];
    mainProgram = "comfyui-workflow-mcp";
    platforms = [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ];
  };
})
