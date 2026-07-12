#!/usr/bin/env sh
# Jumbo Build - Installation Script
# Automatically detects OS (macOS/Linux) and architecture,
# builds the release binary, and installs to ~/.local/bin.

set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="$HOME/.local/bin"
BINARY_NAME="jumbo"

# --- Detect OS and architecture ---
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin) OS="macos" ;;
        Linux)  OS="linux" ;;
        *)
            echo "Error: Unsupported operating system: $OS"
            echo "Jumbo Build supports macOS and Linux only."
            exit 1
            ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH="x86_64" ;;
        aarch64|arm64)  ARCH="aarch64" ;;
        *)
            echo "Error: Unsupported architecture: $ARCH"
            exit 1
            ;;
    esac

    echo "Detected platform: $OS ($ARCH)"
}

# --- Check prerequisites ---
check_prerequisites() {
    if ! command -v cargo >/dev/null 2>&1; then
        echo "Error: Rust/Cargo is not installed."
        echo "Install Rust from https://rustup.rs/"
        exit 1
    fi
}

# --- Build release binary ---
build_binary() {
    echo "Building jumbo in release mode..."
    cd "$REPO_ROOT"
    cargo build --release
    BINARY_PATH="$REPO_ROOT/target/release/$BINARY_NAME"

    if [ ! -f "$BINARY_PATH" ]; then
        echo "Error: Build failed - binary not found at $BINARY_PATH"
        exit 1
    fi
    echo "Build successful."
}

# --- Install binary ---
install_binary() {
    mkdir -p "$INSTALL_DIR"
    cp "$BINARY_PATH" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"
    echo "Installed to $INSTALL_DIR/$BINARY_NAME"
}

# --- Configure PATH ---
configure_path() {
    # Check if already in PATH
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            echo "$INSTALL_DIR is already in PATH."
            return
            ;;
    esac

    echo "Adding $INSTALL_DIR to PATH..."

    # Detect shell and add to appropriate config file
    SHELL_NAME="$(basename "$SHELL")"
    case "$SHELL_NAME" in
        zsh)  SHELL_RC="$HOME/.zshrc" ;;
        bash) SHELL_RC="$HOME/.bashrc" ;;
        fish)
            echo "Fish shell detected. Run this manually:"
            echo "  fish_add_path $INSTALL_DIR"
            return
            ;;
        *)
            echo "Warning: Could not detect shell type. Please add manually:"
            echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
            return
            ;;
    esac

    EXPORT_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
    if [ -f "$SHELL_RC" ]; then
        if ! grep -qF "$INSTALL_DIR" "$SHELL_RC"; then
            echo "" >> "$SHELL_RC"
            echo "# Jumbo Build" >> "$SHELL_RC"
            echo "$EXPORT_LINE" >> "$SHELL_RC"
            echo "Added to $SHELL_RC"
        else
            echo "$INSTALL_DIR already configured in $SHELL_RC"
        fi
    else
        echo "$EXPORT_LINE" >> "$SHELL_RC"
        echo "Created $SHELL_RC with PATH configuration"
    fi

    echo "Run 'source $SHELL_RC' or open a new terminal to use jumbo."
}

# --- Configure shell completions ---
configure_completions() {
    SHELL_NAME="$(basename "$SHELL")"
    BINARY="$INSTALL_DIR/$BINARY_NAME"

    case "$SHELL_NAME" in
        zsh)
            SHELL_RC="$HOME/.zshrc"
            COMPLETE_LINE="source <(COMPLETE=zsh $BINARY)"
            ;;
        bash)
            SHELL_RC="$HOME/.bashrc"
            COMPLETE_LINE="source <(COMPLETE=bash $BINARY)"
            ;;
        fish)
            SHELL_RC="$HOME/.config/fish/completions/$BINARY_NAME.fish"
            COMPLETE_LINE="COMPLETE=fish $BINARY | source"
            mkdir -p "$(dirname "$SHELL_RC")"
            ;;
        *)
            echo "Shell completions: unsupported shell '$SHELL_NAME'. Skipping."
            echo "  You can enable manually — see: jumbo completions --help"
            return
            ;;
    esac

    if [ -f "$SHELL_RC" ] && grep -qF "COMPLETE=" "$SHELL_RC" && grep -qF "$BINARY_NAME" "$SHELL_RC"; then
        echo "Shell completions already configured in $SHELL_RC"
    else
        echo "" >> "$SHELL_RC"
        echo "# Jumbo Build shell completions" >> "$SHELL_RC"
        echo "$COMPLETE_LINE" >> "$SHELL_RC"
        echo "Shell completions ($SHELL_NAME) configured in $SHELL_RC"
    fi
}

# --- Main ---
main() {
    echo "=== Jumbo Build Installer ==="
    echo ""
    detect_platform
    check_prerequisites
    build_binary
    install_binary
    configure_path
    configure_completions
    echo ""
    echo "=== Installation complete! ==="
    echo "Run 'jumbo --help' to get started."
    echo "Open a new terminal or run 'source ~/.zshrc' (or equivalent) to enable completions."
}

main
