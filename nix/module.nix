# NixOS module for `upst record`, the history recorder upst loads charts
# and events from, and /etc/upst/config.toml. Import
# `upst.nixosModules.default` and set `services.upst.enable = true;`.
self:
{ config, lib, pkgs, utils, ... }:

let
	cfg = config.services.upst;
	toml = pkgs.formats.toml { };
	socket = "/run/upst/upst.sock";
	configFile = toml.generate "upst.toml" cfg.settings;
	inherit (lib) mkEnableOption mkIf mkOption types;
in
{
	options.services.upst = {
		enable = mkEnableOption "upst record, which keeps UPS history for the upst dashboard";

		package = mkOption {
			type = types.package;
			default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
			defaultText = lib.literalExpression "upst.packages.\${pkgs.stdenv.hostPlatform.system}.default";
			description = "The upst package. It is also added to the system packages.";
		};

		settings = mkOption {
			type = types.submodule {
				freeformType = toml.type;
				options.record = mkOption {
					type = types.submodule {
						freeformType = toml.type;
						options.listen = mkOption {
							type = types.listOf types.str;
							default = [ ];
							example = [ "0.0.0.0:3494" ];
							apply = l: lib.unique ([ socket ] ++ l);
							description = ''
								Addresses to serve history on besides ${socket}, which is
								always served, for upst on other machines
								(`upst --recorder this-host`). The firewall is left alone.
							'';
						};
					};
					default = { };
					description = "The recorder's settings: `targets`, `interval`, `keep`, `listen`, `timeout`.";
				};
			};
			default = { };
			example = {
				targets = [ "ups@localhost" ];
				charge-style = "lcd";
				ups."ups@localhost".capacity = 1024;
				record = {
					keep = "30d";
					listen = [ "0.0.0.0:3494" ];
				};
			};
			description = ''
				/etc/upst/config.toml: defaults for everyone's upst, which
				~/.config/upst/config.toml and the command line override, and the
				recorder's settings under `record`. Keys are named after the
				command-line options; see
				https://github.com/Dolfost/upst/blob/main/docs/setup.md#the-config-file.
				The journals always go to /var/lib/upst.
			'';
		};

		extraArgs = mkOption {
			type = types.listOf types.str;
			default = [ ];
			description = "More arguments for `upst record`.";
		};
	};

	config = mkIf cfg.enable {
		assertions = [ {
			assertion = !(cfg.settings.record ? dir);
			message = "services.upst: the journals live in /var/lib/upst; drop settings.record.dir.";
		} ];

		environment.systemPackages = [ cfg.package ];
		environment.etc."upst/config.toml".source = configFile;

		systemd.services.upst = {
			description = "upst UPS history recorder";
			documentation = [ "https://github.com/Dolfost/upst" ];
			wantedBy = [ "multi-user.target" ];
			# It reconnects on its own; this only spares a failed first attempt.
			after = [ "network.target" "upsd.service" ];

			serviceConfig = {
				# The store path rather than /etc, so a settings change restarts it.
				ExecStart = utils.escapeSystemdExecArgs (
					[ (lib.getExe cfg.package) "record" "--config" configFile ] ++ cfg.extraArgs
				);
				Restart = "on-failure";
				RestartSec = 5;

				DynamicUser = true;
				StateDirectory = "upst";
				# upst finds the socket here; everyone may read the history,
				# like everyone may ask upsd.
				RuntimeDirectory = "upst";
				RuntimeDirectoryMode = "0755";

				CapabilityBoundingSet = "";
				LockPersonality = true;
				MemoryDenyWriteExecute = true;
				NoNewPrivileges = true;
				PrivateDevices = true;
				PrivateTmp = true;
				PrivateUsers = true;
				ProcSubset = "pid";
				ProtectClock = true;
				ProtectControlGroups = true;
				ProtectHome = true;
				ProtectHostname = true;
				ProtectKernelLogs = true;
				ProtectKernelModules = true;
				ProtectKernelTunables = true;
				ProtectProc = "invisible";
				ProtectSystem = "strict";
				RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ];
				RestrictNamespaces = true;
				RestrictRealtime = true;
				RestrictSUIDSGID = true;
				SystemCallArchitectures = "native";
				SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ];
				UMask = "0022";
			};
		};
	};
}
