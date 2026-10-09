# Container images

Three images, all published to GHCR under the registry in `[images]`:

| Image | Built from | Runs |
|---|---|---|
| `<registry>/washburn-judge:<tag>` | `docker/Dockerfile.rust --target judge` | `washburn-judge` (crawler VM, port 8081 on the compose network) |
| `<registry>/washburn-sweeper:<tag>` | `docker/Dockerfile.rust --target sweeper` | `washburn-sweeper` (crawler VM, N replicas) |
| `<registry>/washburn-web:<tag>` | `web/Dockerfile` (`WASHBURN_APP=public\|admin`) | web VM |

`Dockerfile.rust` builds the whole Cargo workspace once; the `judge` and `sweeper` targets each copy in only their own binary. Both final images are `debian:bookworm-slim` plus `ca-certificates`, `tzdata` and `wget`, and run as uid/gid 10001.

## Build

From the repository root:

```sh
scripts/build-images.sh                  # build and load into the local daemon
scripts/build-images.sh --dry-run        # print the docker buildx commands only
scripts/build-images.sh --push           # build and push to the registry
scripts/build-images.sh --platform linux/arm64   # override the default linux/amd64
```

The script reads `[images].registry` and `[images].tag` from `washburn.toml` (set `WASHBURN_TOML` to use another path). Without the file it falls back to `ghcr.io/OWNER` and `latest` with a warning. `--push` refuses to run while the registry still contains `OWNER`. Each image gets the tag from `[images].tag` and, when the repo has commits, the git short SHA.

To build one target by hand:

```sh
docker buildx build -f docker/Dockerfile.rust --target judge   -t washburn-judge   .
docker buildx build -f docker/Dockerfile.rust --target sweeper -t washburn-sweeper .
```

Notes:

- The build context is the repository root. `.dockerignore` there excludes secrets (`washburn.toml`, `*.local.toml`, `**/*.env`), `web/`, `infra/`, `deploy/`, `docs/`, `target/` and `.git/`. It keeps `Cargo.*`, `crates/` and `db/`. `db/migrations` is embedded at compile time by `sqlx::migrate!`, so it must stay in the context.
- The Rust build uses BuildKit cache mounts for the cargo registry and `target/`. The first build is slow; rebuilds reuse them.
- Cross-building `linux/amd64` on an arm machine works through emulation but is slow. The Rust build is the slowest step.

## Registry login (GHCR)

Use a token with `read:packages` for pulls, or `write:packages` for pushes. Pass it on stdin, never as an argument:

```sh
echo "$GHCR_TOKEN" | docker login ghcr.io -u <github-user> --password-stdin
```

The VMs only need a read-only pull token. `deploy.sh` runs the same `docker login` on each VM when `[images].ghcr_user` and `ghcr_token` are set.

## How deploy env files reference the images

`scripts/render-config.py` computes `<registry>/washburn-<role>:<tag>` from `[images]` and writes it into the rendered env files:

- `WASHBURN_IMAGE_SWEEPER` and `WASHBURN_IMAGE_JUDGE` go into `deploy/compose/crawler.env`, read by `deploy/compose/crawler.yml`.
- `WASHBURN_IMAGE_WEB` goes into `deploy/compose/web.env`.

Compose then pulls them on `docker compose up -d --pull always`. The env files use `[images].tag` only, so to roll out a build either push a new image under that tag and redeploy (`--pull always` re-pulls it), or set `[images].tag` to a specific tag, re-render, and redeploy.

## Uid 10001 and config ownership (D15)

- Every image sets `USER 10001:10001`. The group and user `washburn` are created with uid/gid 10001 in the image.
- The config file is bind-mounted read-only at `/etc/washburn/washburn.toml`, and `WASHBURN_CONFIG` points there. Images also set `WASHBURN_LOG_JSON=1`.
- On the VM, `deploy.sh` installs the file as `root:10001` with mode `0640`. The root-owned file is readable by the container through group 10001 and is not world-readable.
- If the container cannot read the config, the cause is almost always host ownership. Check with `ls -l /etc/washburn/washburn.toml` on the VM. It should be `root:10001 -rw-r-----`.
- The judge reads an optional rubric from `WASHBURN_RUBRIC`, or `/etc/washburn/rubric.md` when present. Nothing in this repo mounts that file yet.

## Healthcheck

The judge image includes `wget` for the compose healthcheck: `wget -qO- http://127.0.0.1:8081/healthz`.
