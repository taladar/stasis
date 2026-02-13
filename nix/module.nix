{ package }:
{ config, lib, pkgs, ... }:

let
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkIf
    getExe
    mkOption
    types
    escapeShellArgs
    optional
    optionals
    literalExpression
    ;

  cfg = config.services.stasis;

  defaultArgs = optionals (cfg.extraConfig != null) [
    "-c"
    "/etc/stasis/stasis.rune"
  ];

  # systemd "Path=" expects directories, so use /bin explicitly for store packages.
  defaultServicePath = [
    "/run/current-system/sw/bin"
    "/etc/profiles/per-user/%u/bin"
    "/nix/var/nix/profiles/default/bin"
    "${pkgs.bash}/bin"
    "${pkgs.coreutils}/bin"
    "${pkgs.systemd}/bin"
  ];
in
{
  options = {
    services.stasis = {
      enable = mkEnableOption "Stasis, a lightweight, feature rich Wayland idle manager written in Rust";

      package = mkPackageOption { stasis = package; } "stasis" { };

      extraConfig = mkOption {
        type = types.nullOr types.lines;
        default = null;
        description = ''
          The literal contents of the Stasis configuration file.

          If set, Nix will write this text to:
          `/etc/stasis/stasis.rune`.
        '';
        example = literalExpression ''
          # (example omitted)
        '';
      };

      target = mkOption {
        type = types.nonEmptyStr;
        default = "graphical-session.target";
        description = "The systemd user target after which Stasis is started.";
      };

      extraArgs = mkOption {
        type = types.listOf types.str;
        default = defaultArgs;
        defaultText = literalExpression ''
          if services.stasis.extraConfig != null then
          [
            "-c"
            "/etc/stasis/stasis.rune"
          ] else []
        '';
        description = ''
          Extra arguments to pass to Stasis.

          The default arguments from this module will be **ignored** if you override this.
        '';
      };

      environmentFile = mkOption {
        type = types.nullOr types.str;
        default = "%h/.config/stasis/stasis.env";
        description = ''
          Optional environment file read by the Stasis systemd user service.
          Useful for compositor-specific variables like NIRI_SOCKET.
          Set to null to disable.
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.stasis = {
      description = "Stasis Wayland Idle Manager";
      after = [ cfg.target ];
      partOf = [ cfg.target ];
      wantedBy = [ cfg.target ];

      restartTriggers = optional (cfg.extraConfig != null)
        config.environment.etc."stasis/stasis.rune".source;

      path = defaultServicePath;

      serviceConfig =
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
          Restart = "on-failure";

          # Only passes vars that exist in the systemd --user manager environment.
          PassEnvironment = [
            "NIRI_SOCKET"
            "WAYLAND_DISPLAY"
            "XDG_RUNTIME_DIR"
          ];
        }
        // (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = [ "-${cfg.environmentFile}" ];
        });
    };

    environment.etc."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
