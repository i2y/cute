# Reproducible Linux build + smoke-test environment for Cute.
#
# Tumbleweed is rolling-release and ships Qt 6.11 (Cute requires
# Qt 6.9+ for the `qt_create_metaobjectdata` template specialization
# in `QtCore/qtmochelpers.h` — Ubuntu LTS' Qt 6.4 is too old).
#
# Usage:
#   docker build -t cute-test .
#   docker run --rm cute-test cute build /cute/examples/counter/counter.cute
FROM opensuse/tumbleweed:latest

RUN zypper --non-interactive refresh \
    && zypper --non-interactive install -y \
        gcc-c++ \
        cmake \
        ninja \
        pkgconf \
        git \
        curl \
        rust \
        cargo \
        qt6-base-devel \
        qt6-declarative-devel \
        qt6-httpserver-devel \
        qt6-charts-devel \
        qt6-svg-devel \
        qt6-multimedia-devel \
        Mesa-libGL-devel \
        libxkbcommon-devel \
    && zypper clean --all

WORKDIR /cute
COPY . /cute

# Workspace test suite — pure Rust, no Qt link required.
RUN cargo test --workspace --quiet 2>&1 | tee /tmp/test_output.log \
    && grep -E '^test result:' /tmp/test_output.log | \
       awk -F'[ .;]+' '{ p+=$4; f+=$6 } END { print "TOTAL:", p, "passed,", f, "failed" }'

RUN cargo install --path crates/cute-cli --quiet
ENV PATH="/root/.cargo/bin:${PATH}"

# Smoke: parse + type-check across archetypes (no codegen, no link).
RUN cute check examples/counter/counter.cute \
    && cute check examples/http_hello/http_hello.cute \
    && cute check examples/widgets_counter/widgets_counter.cute \
    && cute check examples/charts/charts.cute

# End-to-end: codegen + cmake + Qt6 link → native ELF.
RUN cd examples/counter && cute build counter.cute && file counter && cd /cute \
    && cd examples/widgets_counter && cute build widgets_counter.cute \
    && file widgets_counter && cd /cute \
    && cd examples/http_hello && cute build http_hello.cute \
    && file http_hello && cd /cute

# Headless run check: launch the http server, GET /, expect a body.
RUN cd examples/http_hello \
    && (./http_hello 2>/tmp/stderr &) \
    && for i in 1 2 3 4 5; do \
         sleep 0.5; \
         PORT=$(grep -oE '[0-9]+$' /tmp/stderr | tail -1); \
         [ -n "$PORT" ] && break; \
       done \
    && curl -fsS "http://127.0.0.1:$PORT/" \
    && pkill http_hello || true

CMD ["bash"]
