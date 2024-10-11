{
  config,
  pkgs,
  lib,
  ...
}: let
  stdenv = pkgs.stdenv;
  cfg = config.services.wolog;
  toml = pkgs.formats.toml {};
  inherit (lib) mkEnableOption mkPackageOption mkIf mkOption types optional optionalAttrs;
in {
  options.services.wolog = {
    enable = mkEnableOption "wolog";
    package = mkPackageOption pkgs "wolog" {};
    port = mkOption {
      type = types.port;
      default = 8000;
      description = ''
        Listen port for the wolog.
      '';
    };

    address = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = ''
        Listen address for the wolog.
      '';
    };

    articlesDir = mkOption {
      type = types.str;
      default = "/var/lib/wolog/posts";
      description = ''
        The directory where the wolog reads its posts.
      '';
    };

    templatesDir = mkOption {
      type = types.str;
      default = builtins.toString ./templates;
      description = ''
        The directory where the wolog reads its templates.
      '';
    };

    staticDir = mkOption {
      type = types.path;
      default = builtins.toString ./static;
      description = ''
        The directory where the wolog reads its static files.
      '';
    };

    user = mkOption {
      type = types.str;
      default = "wolog";
      description = "User account under which the wolog runs.";
    };

    group = mkOption {
      type = types.str;
      default = "wolog";
      description = "Group account under which the wolog runs.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Open ports in the firewall for the wolog.
      '';
    };

    settings = mkOption {
      type = toml.type;
      default = {};
      description = ''
        Rocket configuration for the wolog.
      '';
    };

    envFile = mkOption {
      type = types.str;
      default = builtins.toString (pkgs.writeText "default.env" "");
    };
  };

  config = let
    settings = toml.generate "Rocket.toml" ({
        release = {
          port = cfg.port;
          address = cfg.address;
        };
      }
      // cfg.settings);
    workdir = stdenv.mkDerivation {
      name = "wolog-workdir";
      unpackPhase = "true";
      installPhase = ''
        mkdir -p $out
        ln -s ${settings} $out/Rocket.toml
        ln -s ${cfg.articlesDir} $out/articles
        ln -s ${cfg.templatesDir} $out/templates
        ln -s ${cfg.staticDir} $out/static
      '';
    };
    wolog = cfg.package + /bin/wolog;
  in
    mkIf cfg.enable {
      systemd.services.wolog = {
        description = "Willow's blog engine";
        after = ["network.target"];
        wantedBy = ["multi-user.target"];

        path = [pkgs.pandoc];

        serviceConfig = {
          Type = "simple";
          User = cfg.user;
          Group = cfg.group;

          EnvironmentFile = cfg.envFile;
          ExecStart = pkgs.writeScript "wolog-start" ''
            #!/bin/sh
            cd ${builtins.toString workdir}
            ${wolog}
          '';
          Restart = "always";
          # BindReadOnlyPaths = "${cfg.articlesDir} ${cfg.templatesDir} ${cfg.staticDir} ${workdir}";
        };
      };
      users.users = optionalAttrs (cfg.user == "wolog") {
        wolog = {
          group = cfg.group;
          isSystemUser = true;
        };
      };

      users.groups = optionalAttrs (cfg.group == "wolog") {
        wolog.members = [cfg.user];
      };
    };
}
