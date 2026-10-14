{
  lib,
  stdenv,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  zlib,
  release,
}:
let
  platform = release.platforms.${stdenv.hostPlatform.system};
in
stdenvNoCC.mkDerivation {
  pname = "fastjj";
  inherit (release) version;

  src = fetchurl {
    url = "https://github.com/Swoorup/fastjj/releases/download/v${release.version}/fastjj-v${release.version}-${platform.target}.tar.gz";
    inherit (platform) hash;
  };

  strictDeps = true;
  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;

  # Rewrite the ELF interpreter and shared-library paths for NixOS. Execute the
  # binary directly: a shell wrapper would expose private arguments in a child.
  nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    stdenv.cc.cc.lib
    zlib
  ];

  installPhase = ''
    runHook preInstall
    install -Dm755 fastjj "$out/bin/fastjj"
    install -Dm755 fastjjui "$out/bin/fastjjui"
    install -Dm644 LICENSE "$out/share/licenses/fastjj/LICENSE"
    install -Dm644 NOTICE "$out/share/licenses/fastjj/NOTICE"
    install -Dm644 licenses/jjui-MIT.txt "$out/share/licenses/fastjj/jjui-MIT.txt"
    install -Dm644 README.md "$out/share/doc/fastjj/README.md"
    install -Dm644 docs/privacy.md "$out/share/doc/fastjj/privacy.md"
    install -Dm644 docs/jjui.md "$out/share/doc/fastjj/jjui.md"
    install -Dm644 build-info.json "$out/share/doc/fastjj/build-info.json"
    runHook postInstall
  '';

  passthru = { inherit (release) sourceCommit; };

  meta = {
    description = "Process-private Jujutsu client and jjui (prebuilt binaries)";
    homepage = "https://github.com/Swoorup/fastjj";
    license = [
      lib.licenses.asl20
      lib.licenses.mit
    ];
    platforms = builtins.attrNames release.platforms;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    mainProgram = "fastjj";
  };
}
