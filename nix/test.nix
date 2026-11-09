# `nix flake check` on Linux boots a VM with a dummy UPS, the recorder
# service and upst, and checks that a normal user gets the history.
# (The strings are indented with spaces: Nix strips only those.)
self: pkgs:

let
	dummy = pkgs.writeText "dummy.dev" ''
    battery.charge: 87
    battery.charge.low: 20
    battery.runtime: 3000
    device.mfr: Test
    device.model: Dummy UPS
    ups.status: OL
  '';
	sock = "socat - UNIX-CONNECT:/run/upst/upst.sock";
in
pkgs.testers.runNixOSTest {
	name = "upst";

	nodes.machine = {
		imports = [ self.nixosModules.default ];
		power.ups = {
			enable = true;
			mode = "standalone";
			ups.dummy = {
				driver = "dummy-ups";
				port = "${dummy}";
				description = "Dummy";
			};
			users.upsmon = {
				passwordFile = "${pkgs.writeText "upsmon-password" "secret"}";
				upsmon = "primary";
			};
			upsmon.monitor.dummy.user = "upsmon";
		};
		services.upst = {
			enable = true;
			settings = {
				format = "{ups} is at {charge}";
				record = {
					targets = [ "dummy@localhost" ];
					keep = "2d";
					listen = [ "127.0.0.1:3494" ];
				};
			};
		};
		users.users.alice.isNormalUser = true;
		environment.systemPackages = [ pkgs.socat pkgs.tmux ];
	};

	testScript = ''
    machine.wait_for_unit("upsd.service")
    machine.wait_for_unit("upst.service")
    machine.wait_until_succeeds("${pkgs.nut}/bin/upsc dummy@localhost ups.status")

    with subtest("a normal user reads the history"):
        machine.wait_until_succeeds("su alice -c 'echo LIST | ${sock}' | grep 'UPS dummy@localhost'")
        machine.wait_until_succeeds("su alice -c 'echo GET dummy@localhost | ${sock}' | grep -x '[0-9]* battery.charge 87'")

    with subtest("the settings reach the recorder and everyone's upst"):
        machine.succeed("journalctl -u upst.service | grep 'kept for 2d'")
        machine.wait_until_succeeds("echo LIST | socat - TCP:127.0.0.1:3494 | grep 'UPS dummy@localhost'")
        machine.succeed("su alice -c 'upst --once' | grep -x 'dummy@localhost is at 87%'")
        machine.succeed("su alice -c 'mkdir -p ~/.config/upst'")
        machine.succeed("printf 'format = \"mine {charge}\"\\n' > /home/alice/.config/upst/config.toml")
        machine.succeed("su alice -c 'upst --once' | grep -x 'mine 87%'")
        machine.succeed("rm /home/alice/.config/upst/config.toml")

    with subtest("history survives a restart"):
        machine.succeed("systemctl restart upst.service")
        machine.wait_for_unit("upst.service")
        machine.wait_until_succeeds("su alice -c 'echo GET dummy@localhost | ${sock}' | grep -c '!start' | grep -qx 2")
        machine.succeed("test -n \"$(ls /var/lib/upst/)\"")

    with subtest("upst finds the recorder by itself"):
        print(machine.succeed("su alice -c 'upst --once dummy@localhost'"))
        machine.succeed("su alice -c 'tmux new-session -d -s t -x 120 -y 40 upst'")
        machine.wait_until_succeeds("su alice -c 'tmux capture-pane -p -t t' | grep 'Recorded history loaded'", timeout=30)
        print(machine.succeed("su alice -c 'tmux capture-pane -p -t t'"))
        machine.succeed("su alice -c 'tmux kill-server'")

    print(machine.succeed("journalctl -u upst.service --no-pager"))
  '';
}
