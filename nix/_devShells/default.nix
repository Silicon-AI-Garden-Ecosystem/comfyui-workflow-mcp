{
  pkgs,
}:

let
  inherit (pkgs.dprint-plugins)
    dprint-plugin-json
    dprint-plugin-toml
    ;

  dprintSettings = pkgs.writeText "dprint.json" (
    builtins.toJSON {
      includes = [
        "**/*.json"
        "**/*.jsonc"
        "**/*.toml"
      ];
      plugins = [
        "${dprint-plugin-json}/plugin.wasm"
        "${dprint-plugin-toml}/plugin.wasm"
      ];
    }
  );
in
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    clippy
    dprint
    just
    just-lsp
    nixd
    nixfmt
    nixfmt-tree
    rom
    rust-analyzer
    rustc
    rustfmt
  ];

  shellHook = ''
    ln -sfn ${dprintSettings} dprint.json
  '';
}
