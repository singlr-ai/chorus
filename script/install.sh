#!/usr/bin/env sh
set -eu

# Downloads a Mast release bundle and unpacks it into ~/.local/.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    ZED_VERSION="${ZED_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/mast-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/mast-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-armhf | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86* | linux-i686*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v mast)" = "$HOME/.local/bin/mast" ]; then
        echo "Mast has been installed. Run with 'mast'"
    else
        echo "To run Mast from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Mast now, '~/.local/bin/mast'"
    fi
}

release_base_url() {
    if [ "$ZED_VERSION" = "latest" ]; then
        if [ "$channel" = "stable" ]; then
            echo "https://github.com/singlr-ai/chorus/releases/latest/download"
        else
            echo "https://github.com/singlr-ai/chorus/releases/download/$channel"
        fi
    elif [ "$channel" = "stable" ]; then
        echo "https://github.com/singlr-ai/chorus/releases/download/v$ZED_VERSION"
    else
        echo "https://github.com/singlr-ai/chorus/releases/download/${channel}-v$ZED_VERSION"
    fi
}

linux() {
    if [ -n "${ZED_BUNDLE_PATH:-}" ]; then
        cp "$ZED_BUNDLE_PATH" "$temp/mast-linux-$arch.tar.gz"
    else
        echo "Downloading Mast version: $ZED_VERSION"
        curl "$(release_base_url)/mast-linux-$arch.tar.gz" > "$temp/mast-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="ai.singlr.Mast"
        ;;
      nightly)
        appid="ai.singlr.Mast-Nightly"
        ;;
      preview)
        appid="ai.singlr.Mast-Preview"
        ;;
      dev)
        appid="ai.singlr.Mast-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="ai.singlr.Mast"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/mast$suffix.app"
    mkdir -p "$HOME/.local/mast$suffix.app"
    tar -xzf "$temp/mast-linux-$arch.tar.gz" -C "$HOME/.local/"

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/mast$suffix.app/bin/mast" ]; then
        ln -sf "$HOME/.local/mast$suffix.app/bin/mast" "$HOME/.local/bin/mast"
    else
        ln -sf "$HOME/.local/mast$suffix.app/bin/cli" "$HOME/.local/bin/mast"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/mast$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        cp "$src_dir/mast$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=mast|Icon=$HOME/.local/mast$suffix.app/share/icons/hicolor/512x512/apps/mast.png|g" "${desktop_file_path}"
    sed -i "s|Exec=mast|Exec=$HOME/.local/mast$suffix.app/bin/mast|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Mast version: $ZED_VERSION"
    curl "$(release_base_url)/Mast-$arch.dmg" > "$temp/Mast-$arch.dmg"
    hdiutil attach -quiet "$temp/Mast-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/mast"
}

main "$@"
