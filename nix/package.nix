{ lib, rustPlatform }:

let
	manifest = (lib.importTOML ../Cargo.toml).package;
in
rustPlatform.buildRustPackage {
	pname = manifest.name;
	inherit (manifest) version;

	# Only what cargo needs (a test reads the example config), so README or
	# flake edits do not rebuild.
	src = lib.fileset.toSource {
		root = ../.;
		fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../contrib/config.toml ];
	};

	cargoLock.lockFile = ../Cargo.lock;

	meta = {
		inherit (manifest) description;
		homepage = "https://github.com/Dolfost/upst";
		license = lib.licenses.asl20;
		mainProgram = "upst";
		platforms = lib.platforms.unix;
	};
}
