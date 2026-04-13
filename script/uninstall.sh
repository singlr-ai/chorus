#!/usr/bin/env sh
set -eu

# Uninstalls Chorus that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        remaining=$(ls -d /Applications/Chorus*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        remaining=$(ls -d "$HOME/.local/chorus"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Chorus preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/chorus"
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

    echo "Chorus has been uninstalled"
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
        appid="ai.singlr.Chorus"
        db_suffix="stable"
        ;;
      nightly)
        appid="ai.singlr.Chorus-Nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="ai.singlr.Chorus-Preview"
        db_suffix="preview"
        ;;
      dev)
        appid="ai.singlr.Chorus-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="ai.singlr.Chorus"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/chorus$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/chorus"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/chorus/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/chorus/chorus-$db_suffix.sock"

    # Remove the entire Chorus directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/chorus"
        prompt_remove_preferences
    fi

    rm -rf $HOME/.chorus_server
}

macos() {
    app="Chorus.app"
    db_suffix="stable"
    app_id="ai.singlr.Chorus"
    case "$channel" in
      nightly)
        app="Chorus Nightly.app"
        db_suffix="nightly"
        app_id="ai.singlr.Chorus-Nightly"
        ;;
      preview)
        app="Chorus Preview.app"
        db_suffix="preview"
        app_id="ai.singlr.Chorus-Preview"
        ;;
      dev)
        app="Chorus Dev.app"
        db_suffix="dev"
        app_id="ai.singlr.Chorus-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/chorus"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Chorus/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Chorus directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Chorus"
        rm -rf "$HOME/Library/Logs/Chorus"

        prompt_remove_preferences
    fi

    rm -rf $HOME/.chorus_server
}

main "$@"
