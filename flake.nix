{
  # UNTESTED: this flake installs the PREBUILT origin release binary per system.
  # It is NOT a from-source build — origin-mem links ONNX Runtime (ort), whose
  # prebuilt native libs cannot be fetched/compiled inside the Nix sandbox, so a
  # source build would fail. Instead we fetch the raw release binary attached to
  # the GitHub Release and patch it to run on NixOS (Linux) via autoPatchelfHook.
  #
  #   nix profile install github:Kantosaurus/origin
  #
  # Version + per-system URLs/hashes are NOT hand-edited: they live in
  # packaging/nix/flake-sources.json, which the release pipeline stamps from the
  # release template (packaging/nix/flake-sources.json.tmpl) on every tag and
  # commits back to the dev branch (see the `nix-sources-update` job in
  # .github/workflows/release.yml). The committed JSON is what `github:` resolves.
  description = "Performance-first agentic coding harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      # Read the stamped sources. Until the first stamped commit lands this file
      # may not exist; we fall back to an empty source set so `nix flake show`
      # still evaluates instead of throwing.
      sourcesPath = ./packaging/nix/flake-sources.json;
      sources =
        if builtins.pathExists sourcesPath then
          builtins.fromJSON (builtins.readFile sourcesPath)
        else
          {
            version = "0.0.0";
            systems = { };
          };

      # Only the systems that actually have a built release binary. Intel macOS
      # (x86_64-darwin) is intentionally absent: there is NO x86_64-apple-darwin
      # build (ort rc.12 dropped that prebuilt); Intel Macs run the aarch64
      # binary via Rosetta 2, which `nix profile install` cannot do for them, so
      # x86_64-darwin is left unsupported by this flake (see notes below).
      supportedSystems = builtins.attrNames sources.systems;

      forEachSystem =
        f: nixpkgs.lib.genAttrs supportedSystems (system: f system nixpkgs.legacyPackages.${system});

      mkOrigin =
        system: pkgs:
        let
          src = sources.systems.${system};
          isLinux = pkgs.stdenv.hostPlatform.isLinux;
        in
        pkgs.stdenv.mkDerivation {
          pname = "origin";
          version = sources.version;

          src = pkgs.fetchurl {
            url = src.url;
            # The release assets are RAW binaries (not archives). Nix accepts a
            # bare lowercase hex sha256 string in the `sha256` attribute (the
            # legacy, still-supported form); the stamper emits hex.
            sha256 = src.sha256;
          };

          # `src` is a single binary file, not an archive: skip the unpack phase.
          dontUnpack = true;

          # autoPatchelfHook is Linux-only: it rewrites the ELF interpreter +
          # RPATH so the glibc-linked release binary runs on NixOS (which has no
          # /lib64/ld-linux). On Darwin it is a no-op and not needed, so we omit
          # it there to keep the derivation evaluating cleanly.
          nativeBuildInputs = pkgs.lib.optionals isLinux [ pkgs.autoPatchelfHook ];

          # Shared libs the patched binary needs at runtime. stdenv.cc.cc.lib
          # provides libstdc++/libgcc_s; zlib is pulled in transitively by the
          # bundled ONNX Runtime. autoPatchelfHook errors loudly if anything
          # else is missing, which surfaces the exact missing lib to add here.
          buildInputs = pkgs.lib.optionals isLinux [
            pkgs.stdenv.cc.cc.lib
            pkgs.zlib
          ];

          installPhase = ''
            runHook preInstall
            install -Dm755 "$src" "$out/bin/origin"
            runHook postInstall
          '';

          # The binary is already stripped + LTO'd by the release build; don't
          # let Nix's strip/RPATH-shrink phases touch the patched ELF.
          dontStrip = true;
          dontPatchELF = true;

          meta = with pkgs.lib; {
            description = "Performance-first agentic coding harness";
            homepage = "https://github.com/Kantosaurus/origin";
            license = licenses.asl20;
            mainProgram = "origin";
            platforms = supportedSystems;
            # Prebuilt, redistributable binary — flag it so it is allowed to be
            # fetched rather than built from source.
            sourceProvenance = [ sourceTypes.binaryNativeCode ];
          };
        };
    in
    {
      packages = forEachSystem (
        system: pkgs: {
          origin = mkOrigin system pkgs;
          default = mkOrigin system pkgs;
        }
      );

      apps = forEachSystem (
        system: pkgs:
        let
          origin = mkOrigin system pkgs;
        in
        {
          origin = {
            type = "app";
            program = "${origin}/bin/origin";
          };
          default = {
            type = "app";
            program = "${origin}/bin/origin";
          };
        }
      );

      formatter = forEachSystem (system: pkgs: pkgs.nixfmt-rfc-style);
    };
}
