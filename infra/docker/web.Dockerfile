# memoire web debugger for container platforms such as Dokploy.
# Build context: the repository root. The build calls /v1 on its own origin;
# the platform proxy routes /v1 and /health on that origin to the gateway.
FROM node:24-bookworm-slim AS build
RUN corepack enable
WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM caddy:2-alpine
COPY infra/docker/web.Caddyfile /etc/caddy/Caddyfile
COPY --from=build /src/web/dist/memoire-web/browser /srv
EXPOSE 8080
