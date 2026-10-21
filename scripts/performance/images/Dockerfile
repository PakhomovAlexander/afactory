# Toolchain source is the verified linux/amd64 manifest, not a mutable tag.
FROM rust:1.88.0-bookworm@sha256:4727898c104ecd2e22d780925832502faee9fe4e70581b8572af081370b315a0 AS gnu
RUN apt-get update && apt-get install -y --no-install-recommends python3 strace linux-perf \
    && rm -rf /var/lib/apt/lists/* \
    && rustc --version | grep '^rustc 1.88.0 '
# Samples have private homes and the builder mounts its Cargo cache at /cargo.
# Tools remain installed outside both; the image never contains repository credentials.
ENV RUSTUP_HOME=/usr/local/rustup

FROM gnu AS musl
RUN rustup target add x86_64-unknown-linux-musl \
    && cargo install --locked cargo-zigbuild --version 0.23.4 \
    && curl --fail --location --retry 3 --max-time 180 \
       https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz -o /tmp/zig.tar.xz \
    && echo '02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239  /tmp/zig.tar.xz' | sha256sum -c - \
    && mkdir /opt/zig && tar -xJf /tmp/zig.tar.xz --strip-components=1 -C /opt/zig \
    && rm /tmp/zig.tar.xz \
    && ln -s /opt/zig/zig /usr/local/bin/zig \
    && test "$(zig version)" = 0.15.2 \
    && cargo-zigbuild --version | grep '0.23.4'
