# memoire landing page for container platforms such as Dokploy.
# Build context: the repository root. site/serve.mjs serves the static build
# with its own compression, caching and security headers.
FROM node:24-bookworm-slim AS build
RUN corepack enable
WORKDIR /src/site
COPY site/package.json site/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY site/ ./
RUN pnpm build

FROM node:24-bookworm-slim
WORKDIR /srv/site
COPY --from=build /src/site/dist ./dist
COPY site/serve.mjs ./serve.mjs
ENV HOST=0.0.0.0 PORT=8080 NODE_ENV=production
USER node
EXPOSE 8080
CMD ["node", "serve.mjs"]
