{ package }:
{ config, lib, pkgs, ... }:

let
  inherit (lib)
    mkIf
    mkEnableOption
    mkPackageOption
    mkOption
    types
    optional
    escapeShellArgs
    getExe
    literalExpression
    ;

  cfg = config.services.stasis;

  defaultServicePath = [
    "/run/current-system/sw"
    "/etc/profiles/per-user/%u"
    "/nix/var/nix/profiles/default"
    pkgs.bash
    pkgs.coreutils
    pkgs.systemd
  ];
in
{
  options.services.stasis = {
    enable = mkEnableOption "Stasis, a lightweight, feature rich Wayland idle manager written in Rust";

    package = mkPackageOption { stasis = package; } "stasis" { };

    extraConfig = mkOption {
      type = types.nullOr types.lines;
      default = null;
      description = ''
        The literal contents of the Stasis configuration file.

        If set, Home Manager will write this text to
        `~/.config/stasis/stasis.rune`.
      '';
      example = literalExpression ''
        # (example omitted)
      '';
    };

    target = mkOption {
      type = types.nonEmptyStr;
      default = config.wayland.systemd.target;
      description = "The systemd user target after which Stasis is started.";
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra arguments to pass to Stasis.";
    };
  };

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.stasis = {
      Unit = {
        Description = "Stasis Wayland Idle Manager";
        PartOf = [ cfg.target ];
        After = [ cfg.target ];
        X-Restart-Triggers = optional (cfg.extraConfig != null)
          config.xdg.configFile."stasis/stasis.rune".source;
      };

      Service = {
        Type = "simple";
        ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
        Restart = "on-failure";
      };

      path = defaultServicePath;

      Install.WantedBy = [ cfg.target ];
    };

    xdg.configFile."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
