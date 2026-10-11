# Rust cross-platform build helpers

CARGO  := cargo
RUSTUP := rustup

# Rust compilation targets

LINUX_TARGETS   := x86_64-unknown-linux-gnu
WINDOWS_TARGETS := x86_64-pc-windows-gnu

.PHONY: help build build-linux build-windows build-all install-targets install-linux install-windows clean

# Host build

build:
	$(CARGO) build --release

# Install targets automatically before building

build-linux: install-linux
	$(CARGO) build --release --target $(LINUX_TARGETS)

build-windows: install-windows
	$(CARGO) build --release --target $(WINDOWS_TARGETS)

# Build every platform

build-all: build-linux build-windows

# Install targets

install-linux:
	$(RUSTUP) target add $(LINUX_TARGETS)

install-windows:
	$(RUSTUP) target add $(WINDOWS_TARGETS)

install-targets: install-linux install-windows

clean:
	$(CARGO) clean
