{
  description = "L3O Audio — cross-platform audio mixer and DSP suite";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in {
      devShells = forAllSystems (pkgs:
        let
          inherit (pkgs) lib stdenv;
        in {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              pkg-config
              clang
              cmake
              cargo-deny
            ] ++ lib.optionals stdenv.hostPlatform.isLinux [
              pipewire
              alsa-lib
            ];

            shellHook = ''
              export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            '';
          };
        });
    };
}
