# Build stage
FROM rust:1.85 AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    libclang-dev \
    pkg-config \
    libssl-dev \
    git \
    && rm -rf /var/lib/apt/lists/*


WORKDIR /build

# Install ZisK toolchain
RUN curl https://raw.githubusercontent.com/0xPolygonHermez/zisk/main/ziskup/install.sh | bash && \
    ~/.zisk/bin/ziskup --nokey

# Copy only what's needed for the build
COPY . .

# Build the server
RUN --mount=type=ssh \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --bin validity --release && \
    cp target/release/validity /build/validity-proposer

# Final stage
FROM rust:1.85-slim

WORKDIR /app

# Install required runtime dependencies
RUN apt-get update && apt-get install -y \
    curl \
    clang \
    pkg-config \
    libssl-dev \
    ca-certificates \
    git \
    libclang-dev \
    jq \
    && rm -rf /var/lib/apt/lists/*

# Install ZisK toolchain
RUN curl https://raw.githubusercontent.com/0xPolygonHermez/zisk/main/ziskup/install.sh | bash && \
    ~/.zisk/bin/ziskup --nokey

# Copy only the built binaries from builder
COPY --from=builder /build/validity-proposer /usr/local/bin/validity-proposer

# Run the server from its permanent location
CMD ["/usr/local/bin/validity-proposer"]
