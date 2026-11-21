FROM oven/bun:1.4.5-debian@sha256:3dddea58e1f8f2240eca906b023fedb944f9a8352515a35309fd0c879fe00aaa

USER root

WORKDIR /app/hosted
COPY hosted/server.ts hosted/service.ts hosted/bindings.ts hosted/providers.ts hosted/edge.ts hosted/result.ts ./
COPY dashboard/public/favicon.png /app/dashboard/public/favicon.png
COPY dashboard/app/meadow/desktop.webp dashboard/app/meadow/mobile.webp /app/dashboard/app/meadow/
COPY --chmod=0755 hosted/docker-entrypoint.sh /usr/local/bin/milo-relay-entrypoint

ENV NODE_ENV=production \
    DO_NOT_TRACK=1 \
    MILO_RELAY_ORIGIN=https://connect.usemilo.net \
    MILO_RELAY_DATA_DIR=/data

ENTRYPOINT ["/usr/local/bin/milo-relay-entrypoint"]
CMD ["bun", "run", "--no-env-file", "server.ts"]
