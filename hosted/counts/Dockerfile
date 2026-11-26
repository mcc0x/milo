FROM oven/bun:1.4.5-debian@sha256:3dddea58e1f8f2240eca906b023fedb944f9a8352515a35309fd0c879fe00aaa

WORKDIR /app
COPY server.ts db.ts page.ts schema.sql ./
COPY assets ./assets

ENV NODE_ENV=production \
    DO_NOT_TRACK=1 \
    MILO_COUNTS_HOST=0.0.0.0

USER bun
CMD ["bun", "run", "--no-env-file", "server.ts"]
