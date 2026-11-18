{
  pkgs,
}:

pkgs.mkShell {
  packages = with pkgs; [
    cargo
    clippy
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
}
