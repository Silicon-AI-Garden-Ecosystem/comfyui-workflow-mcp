{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "comfyui-workflow-mcp";
  version = "0.1.0";

  src = ../../..;

  cargoHash = "sha256-EV5IDeCVwWmglm9n/iycGvZ6WKX62S1DW8jN9bf88is=";

  doCheck = true;

  meta = {
    description = "A ComfyUI workflow MCP friendly to both humans and agents";
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
