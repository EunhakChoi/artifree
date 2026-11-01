# Artifree server image (decision 0026).
#
# The build context is NOT the repository: it is an unpacked, verified Linux
# x64 release package (decision 0025), so the image holds exactly the files
# listed in that package's MANIFEST.json and no second build path exists.
#   node scripts/verify-container.mjs <release directory>   # builds and checks
#   docker build -f deploy/container/Dockerfile <unpacked package directory>
#
# Base: Debian 12 slim, pinned by the multi-arch index digest (resolved from
# the registry on 2026-11-01). glibc 2.36 is newer than the 2.34 the packaged
# binary needs. Refresh the digest deliberately; Dependabot is not configured
# for this file.
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

LABEL org.opencontainers.image.title="Artifree server" \
      org.opencontainers.image.source="https://github.com/EunhakChoi/artifree" \
      org.opencontainers.image.licenses="MIT"

# A fixed unprivileged user. /data is created before VOLUME so that a new
# named volume inherits its owner and mode 700, which the server requires.
RUN groupadd --system --gid 10001 artifree \
 && useradd --system --uid 10001 --gid 10001 --no-create-home \
      --home-dir /nonexistent --shell /usr/sbin/nologin artifree \
 && install -d -m 700 -o 10001 -g 10001 /data

COPY . /app

USER 10001:10001
VOLUME ["/data"]
# 7410: API and web workspace. 7411: preview content, a separate origin.
EXPOSE 7410 7411

# bash and /dev/tcp are in the base image, so no package is installed. The
# check follows the in-container API port; keep it at 7410 and publish a
# different host port with -p instead of changing --api-listen.
HEALTHCHECK --interval=10s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/7410 && printf 'GET /healthz HTTP/1.0\\r\\nHost: localhost:7410\\r\\n\\r\\n' >&3 && read -r status <&3 && [[ $status == HTTP/1.?\\ 200* ]]"]

# Arguments given to `docker run <image> ...` are appended to this command.
ENTRYPOINT ["/app/bin/artifree-server", "serve", \
  "--data-dir", "/data", "--web-dir", "/app/web", \
  "--api-listen", "0.0.0.0:7410", "--content-listen", "0.0.0.0:7411"]
