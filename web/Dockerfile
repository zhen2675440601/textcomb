# syntax=docker/dockerfile:1.7
FROM node:24-bookworm-slim AS builder
WORKDIR /src
ARG VITE_TEXTCOMB_SOURCE_URL=https://github.com/textcomb/textcomb
ENV VITE_TEXTCOMB_SOURCE_URL=$VITE_TEXTCOMB_SOURCE_URL
COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --ignore-scripts
COPY . .
RUN npm run build

FROM caddy:2.10-alpine
COPY Caddyfile /etc/caddy/Caddyfile
COPY --from=builder /src/dist /srv
EXPOSE 8080
