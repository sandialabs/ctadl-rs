{
  buildDotnetGlobalTool,
  makeWrapper,
  lib,
}:
buildDotnetGlobalTool {
  pname = "Sarif.Multitool";
  version = "4.6.2";

  nativeBuildInputs = [ makeWrapper ];

  executables = [ "sarif" ];

  nugetSha256 = "sha256-tItudmpmK02XIkULu0KSuzd4kc2w0Es1h4hZ9rcCjNU=";
}
