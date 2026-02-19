{ package }:
{ config, lib, pkgs, ... }:
let
  inherit (lib)
    mkIf
    mkMerge
    mkEnableOption
    mkPackageOption
    mkOption
    types
    escapeShellArgs
    getExe
    literalExpression
    makeBinPath
    ;
  cfg = config.services.stasis;

  # Build an explicit PATH string so we never hit the /bin/bin double-suffix
  # bug that occurs when systemd appends /bin to entries already containing it.
  servicePathPkgs = with pkgs; [
    bashInteractive
    coreutils
    systemd
  ];
  explicitPath =
    "/run/current-system/sw/bin"
    + ":/etc/profiles/per-user/%u/bin"
    + ":/nix/var/nix/profiles/default/bin"
    + ":${makeBinPath servicePathPkgs}";
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
        default:
          lock_screen:
            timeout 300
            command "swaylock"
          end
          suspend:
            timeout 600
            command "systemctl suspend"
          end
        end
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

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.stasis = {
      Unit = {
        Description = "Stasis Wayland Idle Manager";
        PartOf = [ cfg.target ];
        After = [ cfg.target ];
      };

      Service = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
          Restart = "on-failure";
          Slice = "session.slice";
          # Explicit PATH avoids systemd's /bin suffix being appended to paths
          # that already contain /bin, and keeps the environment deterministic.
          Environment = [
            "PATH=${explicitPath}"
          ];
          PassEnvironment = [
            "NIRI_SOCKET"
            "WAYLAND_DISPLAY"
            "XDG_RUNTIME_DIR"
            "DBUS_SESSION_BUS_ADDRESS"
          ];
        }
        # mkMerge (not //) is required here: // is a plain Nix attribute merge
        # and does not understand mkIf — when environmentFile is null, mkIf
        # returns a special lib value rather than {}, breaking the merge.
        (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = [ "-${cfg.environmentFile}" ];
        })
      ];

      Install = {
        WantedBy = [ cfg.target ];
      };
    };

    xdg.configFile."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
