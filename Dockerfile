FROM --platform=$BUILDPLATFORM docker.io/library/rust:1.98.0-slim-trixie AS toolchain
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3-venv \
 && rm -rf /var/lib/apt/lists/* \
 && python3 -m venv /opt/zig \
 && /opt/zig/bin/pip install --no-cache-dir ziglang==0.16.0 cargo-zigbuild==0.23.4
ENV PATH=/opt/zig/bin:$PATH
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup toolchain install \
 && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
COPY . .

FROM toolchain AS build
ARG CARGO_BUILD_JOBS
RUN --mount=type=cache,id=bosun-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=bosun-target,target=/src/target,sharing=locked \
    cargo zigbuild --locked --release -p bosun-server-bin -p bosun-agent \
      --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl \
 && install -D -m 0755 target/x86_64-unknown-linux-musl/release/bosun-server /out/server/amd64/bosun-server \
 && install -D -m 0755 target/aarch64-unknown-linux-musl/release/bosun-server /out/server/arm64/bosun-server \
 && install -D -m 0755 target/x86_64-unknown-linux-musl/release/bosun-agent /out/agent/linux-x86_64 \
 && install -D -m 0755 target/aarch64-unknown-linux-musl/release/bosun-agent /out/agent/linux-aarch64

FROM toolchain AS cli-build
ARG CARGO_BUILD_JOBS
ARG TARGETARCH
RUN --mount=type=cache,id=bosun-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=bosun-target,target=/src/target,sharing=locked \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "there is no bosun for $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && cargo zigbuild --locked --release -p bosun-cli --target "$target" \
 && install -D -m 0755 "target/$target/release/bosun" /out/bosun

FROM scratch AS cli
COPY --from=cli-build /out/bosun /bosun

FROM --platform=$BUILDPLATFORM docker.io/library/alpine:3.24 AS users
RUN addgroup -S -g 210 bosun \
 && adduser -S -D -H -u 210 -G bosun -h /bosun-data -s /sbin/nologin bosun \
 && mkdir /bosun-data

FROM docker.io/library/alpine:3.24
ARG TARGETARCH
ARG REVISION=unknown
LABEL org.opencontainers.image.title="Bosun" \
      org.opencontainers.image.description="A CI/CD server that runs workflows on agents the team owns" \
      org.opencontainers.image.source="https://github.com/GlobalArtInc/bosun-ci" \
      org.opencontainers.image.revision="$REVISION"
COPY --from=users /etc/passwd /etc/group /etc/
COPY --from=users --chown=210:210 /bosun-data /bosun-data
COPY --from=build /out/agent/ /usr/lib/bosun/agent/
COPY --from=build /out/server/$TARGETARCH/bosun-server /usr/local/bin/bosun-server
ENV BOSUN_SERVER_DATA_DIR=/bosun-data
USER 210:210
WORKDIR /bosun-data
VOLUME /bosun-data
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s --retries=3 CMD ["bosun-server", "check"]
ENTRYPOINT ["bosun-server"]
CMD ["serve"]
