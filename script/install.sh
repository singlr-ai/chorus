#!/usr/bin/env sh
set -eu

# Downloads a Chorus release bundle and unpacks it into ~/.local/.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    ZED_VERSION="${ZED_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/chorus-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/chorus-XXXXXX")"
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

    if [ "$(command -v chorus)" = "$HOME/.local/bin/chorus" ]; then
        echo "Chorus has been installed. Run with 'chorus'"
    else
        echo "To run Chorus from your terminal, you must add ~/.local/bin to your PATH"
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

        echo "To run Chorus now, '~/.local/bin/chorus'"
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
        cp "$ZED_BUNDLE_PATH" "$temp/chorus-linux-$arch.tar.gz"
    else
        echo "Downloading Chorus version: $ZED_VERSION"
        curl "$(release_base_url)/chorus-linux-$arch.tar.gz" > "$temp/chorus-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="ai.singlr.Chorus"
        ;;
      nightly)
        appid="ai.singlr.Chorus-Nightly"
        ;;
      preview)
        appid="ai.singlr.Chorus-Preview"
        ;;
      dev)
        appid="ai.singlr.Chorus-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="ai.singlr.Chorus"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/chorus$suffix.app"
    mkdir -p "$HOME/.local/chorus$suffix.app"
    tar -xzf "$temp/chorus-linux-$arch.tar.gz" -C "$HOME/.local/"

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/chorus$suffix.app/bin/chorus" ]; then
        ln -sf "$HOME/.local/chorus$suffix.app/bin/chorus" "$HOME/.local/bin/chorus"
    else
        ln -sf "$HOME/.local/chorus$suffix.app/bin/cli" "$HOME/.local/bin/chorus"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/chorus$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        cp "$src_dir/chorus$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=chorus|Icon=$HOME/.local/chorus$suffix.app/share/icons/hicolor/512x512/apps/chorus.png|g" "${desktop_file_path}"
    sed -i "s|Exec=chorus|Exec=$HOME/.local/chorus$suffix.app/bin/chorus|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Chorus version: $ZED_VERSION"
    curl "$(release_base_url)/Chorus-$arch.dmg" > "$temp/Chorus-$arch.dmg"
    hdiutil attach -quiet "$temp/Chorus-$arch.dmg" -mountpoint "$temp/mount"
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
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/chorus"
}

main "$@"
