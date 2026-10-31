{
  description = "Search the keybindings for the focused window and run the one you pick";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      description = "Search the keybindings for the focused window and run the one you pick";

      keybind-find = pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "keybind-find";
          version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ./data ./crates (pkgs.lib.fileset.maybeMissing ./man) ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          # ponytail: the tests need a pinned nvim (mise.toml) and run in CI (ci.yml), not in the sandbox.
          doCheck = false;

          nativeBuildInputs = [ pkgs.makeWrapper pkgs.installShellFiles ];

          # The X11 tools and fzf go after the user's PATH, so their own versions still come first (an fzf older than
          # 0.66 on the PATH then means no key search). The picker window (alacritty, dmenu, fuzzel), swaymsg,
          # wtype, notify-send and the programs it reads (nvim, tmux, herdr, espanso) stay the user's choice.
          postInstall = ''
            if [ -d man ]; then installManPage man/keybind-find.1; fi
            wrapProgram $out/bin/keybind-find --suffix PATH : ${pkgs.lib.makeBinPath [
              pkgs.xdotool
              (pkgs.xprop or pkgs.xorg.xprop)
              (pkgs.xmodmap or pkgs.xorg.xmodmap)
              pkgs.fzf
            ]}
          '';

          meta = {
            inherit description;
            homepage = "https://github.com/Benjmhart/keybind-find";
            license = pkgs.lib.licenses.mit;
            mainProgram = "keybind-find";
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages.${system}.default = keybind-find pkgs;

      apps.${system}.default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/keybind-find";
        meta.description = description;
      };

      overlays.default = final: _prev: { keybind-find = keybind-find final; };

      # `nix flake check` builds these. With this nixpkgs the overlay gives the same derivation, so `overlay` only
      # shows that it evaluates.
      checks.${system} = {
        default = self.packages.${system}.default;
        overlay = (pkgs.extend self.overlays.default).keybind-find;
      };
    };
}
