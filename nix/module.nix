{ package }:
{ config, lib, pkgs, ... }:
let
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkIf
    mkMerge
    getExe
    mkOption
    types
    escapeShellArgs
    optionals
    literalExpression
    makeBinPath
    ;
  cfg = config.services.stasis;

  defaultArgs = optionals (cfg.extraConfig != null) [
    "-c"
    "/etc/stasis/stasis.rune"
  ];

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
        default = "graphical-session.target";
        description = "The systemd user target after which Stasis is started.";
      };

      extraArgs = mkOption {
        type = types.listOf types.str;
        default = defaultArgs;
        defaultText = literalExpression ''
          if services.stasis.extraConfig != null then
            [ "-c" "/etc/stasis/stasis.rune" ]
          else
            []
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

      # path expects derivations, not literal paths — systemd will append /bin.
      # We also set PATH explicitly in serviceConfig.Environment below so that
      # the ordering is deterministic and system profile bins come first.
      path = servicePathPkgs;

      serviceConfig = mkMerge [
        {
          Type = "simple";
          ExecStart = "${getExe cfg.package} ${escapeShellArgs cfg.extraArgs}";
          Restart = "on-failure";
          # Explicit PATH; keeps environment predictable regardless of the
          # user's profile state at service start time.
          Environment = [
            "PATH=${explicitPath}"
          ];
          PassEnvironment = [
            "NIRI_SOCKET"
            "WAYLAND_DISPLAY"
            "XDG_RUNTIME_DIR"
          ];
        }
        # mkMerge (not //) is required: // is a plain Nix attribute merge and
        # does not understand mkIf. When environmentFile is null, mkIf returns
        # a special lib sentinel rather than {}, which breaks the merge.
        (mkIf (cfg.environmentFile != null) {
          EnvironmentFile = [ "-${cfg.environmentFile}" ];
        })
      ];
    };

    environment.etc."stasis/stasis.rune" = mkIf (cfg.extraConfig != null) {
      text = cfg.extraConfig;
    };
  };
}
