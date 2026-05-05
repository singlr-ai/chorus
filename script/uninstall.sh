#!/usr/bin/env sh
set -eu

# Uninstalls Mast that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        remaining=$(ls -d /Applications/Mast*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        remaining=$(ls -d "$HOME/.local/mast"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Mast preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/mast"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${ZED_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Mast has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="ai.singlr.Mast"
        db_suffix="stable"
        ;;
      nightly)
        appid="ai.singlr.Mast-Nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="ai.singlr.Mast-Preview"
        db_suffix="preview"
        ;;
      dev)
        appid="ai.singlr.Mast-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="ai.singlr.Mast"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/mast$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/mast"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/mast/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/mast/mast-$db_suffix.sock"

    # Remove the entire Mast directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/mast"
        prompt_remove_preferences
    fi

    rm -rf $HOME/.mast_server
}

macos() {
    app="Mast.app"
    db_suffix="stable"
    app_id="ai.singlr.Mast"
    case "$channel" in
      nightly)
        app="Mast Nightly.app"
        db_suffix="nightly"
        app_id="ai.singlr.Mast-Nightly"
        ;;
      preview)
        app="Mast Preview.app"
        db_suffix="preview"
        app_id="ai.singlr.Mast-Preview"
        ;;
      dev)
        app="Mast.app"
        db_suffix="dev"
        app_id="ai.singlr.Mast-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/mast"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Mast/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Mast directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Mast"
        rm -rf "$HOME/Library/Logs/Mast"

        prompt_remove_preferences
    fi

    rm -rf $HOME/.mast_server
}

main "$@"
