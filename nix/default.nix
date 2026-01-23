{
  name,
  version,
  lib,
  rustPlatform,
  wayland,
  wayland-protocols,
  dbus,
  pkg-config,
}:
rustPlatform.buildRustPackage {
  inherit version;
  pname = name;

  src = ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = [
    wayland
    wayland-protocols
    dbus
  ];

  meta = {
    description = "Modern idle manager for Wayland";
    longDescription = ''
      Stasis is a smart idle manager for Wayland that understands context.
      It automatically prevents idle when watching videos, reading documents,
      or playing music, while allowing idle when appropriate. Features include
      media-aware idle handling, application-specific inhibitors, Wayland idle
      inhibitor protocol support, and flexible configuration using the RUNE
      configuration language.
    '';
    homepage = "https://github.com/saltnpepper97/stasis";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = name;
  };
}
