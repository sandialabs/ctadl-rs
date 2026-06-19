{
  description =
    "Reproducible nix wrappers around Operation Mango (mango / env_resolve / mango-pipeline) for the command-injection firmware-eval benchmark.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # --------------------------------------------------------------------
        # Reproducibility anchor.
        #
        # Operation Mango pins `angr==9.2.94` plus a pile of native deps and a
        # git-pinned binwalk; building that env from source under nix is a
        # multi-hour yak-shave that breaks every time upstream angr moves. The
        # honest, content-addressed anchor is the prebuilt image DIGEST, not the
        # mutable `:latest` tag.
        #
        # Pin it once with:
        #     nix run .#mango-pin
        # (prints the current sha256), then paste the digest below. After that,
        # every invocation on every machine runs byte-identical Mango.
        # --------------------------------------------------------------------
        imageRef = "docker/operation-mango:pinned"; # local tag, loaded from store below
        upstreamRef = "cl4sm/operation-mango:latest";
        imageDigest = ""; # e.g. "sha256:deadbeef...."; empty => fall back to :latest (NOT reproducible)

        runImage =
          if imageDigest != "" then "cl4sm/operation-mango@${imageDigest}" else upstreamRef;

        # Pick podman if available, else docker. Mount $PWD at the same path so
        # absolute paths the caller passes (target binary, --results dir) resolve
        # identically inside the container; also mount $MANGO_DATA if exported
        # (point it at the corpus root that lives outside the cwd). Run as the
        # caller's uid so result files are owned by the host user, not root.
        containerRun = pkgs.writeShellApplication {
          name = "mango-container-run";
          runtimeInputs = [ pkgs.coreutils ];
          text = ''
            rt=""
            if command -v podman >/dev/null 2>&1; then rt=podman
            elif command -v docker >/dev/null 2>&1; then rt=docker
            else echo "error: need docker or podman on PATH" >&2; exit 127
            fi

            tool="$1"; shift
            mounts=( -v "$PWD:$PWD" -w "$PWD" )
            if [ -n "''${MANGO_DATA:-}" ]; then mounts+=( -v "$MANGO_DATA:$MANGO_DATA" ); fi

            if [ "''${MANGO_PINNED:-1}" = "1" ] && [ -z "${imageDigest}" ]; then
              echo "warning: running ${upstreamRef} unpinned; set imageDigest in flake.nix for reproducibility" >&2
            fi

            exec "$rt" run --rm \
              --user "$(id -u):$(id -g)" \
              --entrypoint "$tool" \
              "''${mounts[@]}" \
              "${runImage}" "$@"
          '';
        };

        mkWrapper = tool:
          pkgs.writeShellApplication {
            name = tool;
            runtimeInputs = [ containerRun ];
            text = ''exec mango-container-run ${tool} "$@"'';
          };

        mango = mkWrapper "mango";
        envResolve = mkWrapper "env_resolve";
        mangoPipeline = mkWrapper "mango-pipeline";

        # Helper: resolve and print the current digest of :latest so you can pin.
        mangoPin = pkgs.writeShellApplication {
          name = "mango-pin";
          runtimeInputs = [ pkgs.skopeo ];
          text = ''
            echo "Resolving digest for docker.io/${upstreamRef} ..." >&2
            skopeo inspect "docker://docker.io/${upstreamRef}" \
              | ${pkgs.jq}/bin/jq -r '.Digest' \
              | sed 's/^/imageDigest = "/; s/$/";/'
          '';
        };

        # Python env for the benchmark harness (normalizers, sqlite, bench.py).
        pyEnv = pkgs.python311.withPackages (ps: with ps; [ rich ]);

      in
      {
        packages = {
          inherit mango envResolve mangoPipeline mangoPin;
          default = mango;
        };

        apps = {
          mango = flake-utils.lib.mkApp { drv = mango; };
          env-resolve = flake-utils.lib.mkApp { drv = envResolve; name = "env_resolve"; };
          mango-pipeline = flake-utils.lib.mkApp { drv = mangoPipeline; };
          mango-pin = flake-utils.lib.mkApp { drv = mangoPin; };
        };

        devShells.default = pkgs.mkShell {
          packages = [ mango envResolve mangoPipeline mangoPin pyEnv pkgs.sqlite pkgs.jq ];
          shellHook = ''
            echo "firmware-eval: mango / env_resolve / mango-pipeline wrap ${runImage}"
            [ -z "${imageDigest}" ] && echo "  (unpinned — run 'nix run .#mango-pin' and paste the digest into flake.nix)"
          '';
        };
      }
    );
}
