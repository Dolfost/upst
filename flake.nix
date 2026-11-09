{
	description = "upst - terminal dashboard for UPSes monitored by Network UPS Tools";

	inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

	outputs = { self, nixpkgs }:
	let
		inherit (nixpkgs) lib;
		systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
		forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
	in {
		# `nix build`, `nix run`, `nix profile install`.
		packages = forAllSystems (pkgs: rec {
			upst = pkgs.callPackage ./nix/package.nix { };
			default = upst;
		});

		apps = forAllSystems (pkgs: rec {
			upst = {
				type = "app";
				program = lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.upst;
				meta.description = "Terminal dashboard for NUT UPSes";
			};
			default = upst;
		});

		# For a NixOS or home-manager config: `nixpkgs.overlays = [ upst.overlays.default ]`
		# and then `pkgs.upst`.
		overlays.default = final: _prev: {
			upst = final.callPackage ./nix/package.nix { };
		};

		# `services.upst`: the history recorder as a system service.
		nixosModules.default = import ./nix/module.nix self;

		# The package build runs `cargo test`; on Linux a VM test covers the
		# NixOS module too.
		checks = forAllSystems (pkgs: {
			upst = self.packages.${pkgs.stdenv.hostPlatform.system}.upst;
		} // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
			nixos = import ./nix/test.nix self pkgs;
		});

		devShells = forAllSystems (pkgs: {
			default = pkgs.mkShell {
				inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.upst ];
				packages = with pkgs; [ rust-analyzer clippy rustfmt cargo-edit nut ];
				RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
			};
		});

		formatter = forAllSystems (pkgs: pkgs.nixfmt);
	};
}
