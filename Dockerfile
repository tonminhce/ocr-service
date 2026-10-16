FROM rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS builder
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin ocr-api --bin ocr-worker --bin parser-renderer --bin layout-detect --bin ocr-eval --bin ocr-admin --bin ocr-secrets --bin ocr-backup \
    && cargo build --release --locked -p ocr-worker --example mock_vllm --example mock_layout --example make_fixtures \
    && cargo build --release --locked -p ocr-api --example service_client

# Verification-only stage; no test tools/source enter the application image.
FROM builder AS parser-verification
RUN apt-get update \
    && apt-get install -y --no-install-recommends poppler-utils procps \
    && rm -rf /var/lib/apt/lists/*
RUN cargo test --release --locked -p ocr-worker --lib --test inference_contract --test layout_pipeline

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS onnxruntime
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      arm64) ort_arch=aarch64; ort_sha=7c63c73560ed76b1fac6cff8204ffe34fe180e70d6582b5332ec094810241e5c ;; \
      amd64) ort_arch=x64; ort_sha=1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b ;; \
      *) exit 1 ;; \
    esac \
    && curl --fail --location --retry 3 "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-linux-${ort_arch}-1.23.2.tgz" --output /tmp/ort.tgz \
    && echo "$ort_sha  /tmp/ort.tgz" | sha256sum --check - \
    && mkdir -p /opt/onnxruntime \
    && tar --extract --gzip --file /tmp/ort.tgz --directory /opt/onnxruntime --strip-components=1 \
    && rm /tmp/ort.tgz

# Rehearsal-only helper/tests never enter the application or operations image.
FROM builder AS backup-rehearsal-builder
RUN cargo test --release --locked -p ocr-core --bin ocr-backup -- --nocapture \
    && cargo build --release --locked -p ocr-core --example backup_rehearsal

# Isolated one-shot operations image. Default application target stays last.
FROM postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336 AS backup-ops
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends age=1.2.1-1+b5 \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /usr/local/bin/gosu \
    && useradd --uid 10001 --create-home ocr \
    && install -d -o 10001 -g 10001 -m 0700 /scratch /backups
COPY --from=builder /app/target/release/ocr-backup /usr/local/bin/ocr-backup
COPY --from=builder /app/target/release/ocr-admin /usr/local/bin/ocr-admin
COPY --from=builder /app/target/release/ocr-secrets /usr/local/bin/ocr-secrets
USER 10001:10001
ENTRYPOINT []
CMD ["ocr-backup", "--help"]

FROM backup-ops AS backup-rehearsal
COPY --from=backup-rehearsal-builder /app/target/release/examples/backup_rehearsal /usr/local/bin/backup-rehearsal

# Isolated document parser image: no API, worker, model, credentials, or ONNX runtime.
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS parser-runtime
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends poppler-utils \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10002 --no-create-home --shell /usr/sbin/nologin ocr-parser \
    && install -d -o 10002 -g 10002 -m 0700 /scratch
COPY --from=builder /app/target/release/parser-renderer /usr/local/bin/parser-renderer
USER 10002:10001
ENTRYPOINT ["parser-renderer"]

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home ocr
COPY --from=builder /app/target/release/ocr-api /usr/local/bin/ocr-api
COPY --from=builder /app/target/release/ocr-worker /usr/local/bin/ocr-worker
COPY --from=builder /app/target/release/layout-detect /usr/local/bin/layout-detect
COPY --from=builder /app/target/release/ocr-eval /usr/local/bin/ocr-eval
COPY --from=builder /app/target/release/ocr-admin /usr/local/bin/ocr-admin
COPY --from=builder /app/target/release/ocr-secrets /usr/local/bin/ocr-secrets
COPY --from=onnxruntime /opt/onnxruntime/lib /opt/onnxruntime/lib
COPY --from=onnxruntime /opt/onnxruntime/LICENSE /opt/onnxruntime/LICENSE
COPY --from=onnxruntime /opt/onnxruntime/ThirdPartyNotices.txt /opt/onnxruntime/ThirdPartyNotices.txt
ENV ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so.1.23.2
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/share/doc/ocr-service/
COPY licenses /usr/share/doc/ocr-service/licenses
COPY --from=builder /app/target/release/examples/mock_vllm /usr/local/bin/mock-vllm
COPY --from=builder /app/target/release/examples/mock_layout /usr/local/bin/mock-layout
COPY --from=builder /app/target/release/examples/make_fixtures /usr/local/bin/make-fixtures
COPY --from=builder /app/target/release/examples/service_client /usr/local/bin/service-client
USER 10001:10001
EXPOSE 8080
CMD ["ocr-api"]
