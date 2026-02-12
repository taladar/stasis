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

  # Provide a sane PATH for systemd user services on NixOS.
  # `path` entries get their /bin added automatically for store paths.
  # For string paths, systemd uses them directly (expects .../bin layout).
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

      serviceConfig = {
        Type = "simple";
        ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
        Restart = "on-failure";
      };
    };

    environment.etc."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
