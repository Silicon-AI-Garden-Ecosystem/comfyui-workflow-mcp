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
    nix-output-monitor
    rust-analyzer
    rustc
    rustfmt
  ];
}
