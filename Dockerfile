# syntax=docker/dockerfile:1

# --- Compilation ---
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked && strip target/release/isrobot

# --- Image finale (≈ 100 Mo) ---
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data isrobot \
 && mkdir -p /data && chown isrobot /data
COPY --from=build /src/target/release/isrobot /usr/local/bin/isrobot

# Le bot lit/écrit .env, la base, les logs et les sauvegardes dans son dossier courant.
WORKDIR /data
VOLUME /data
USER isrobot
# Interface web (si WEB_ENABLED=true, avec WEB_BIND=0.0.0.0:8080)
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/isrobot"]
