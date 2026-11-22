#!/bin/sh
# Installs peugeot300mille for the current user: the program, its icon and a menu entry.
# Run it again after changing the code.  To remove: ./install.sh --uninstall
set -e
cd "$(dirname "$0")"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor"

if [ "$1" = "--uninstall" ]; then
    cargo uninstall peugeot300mille || true
    rm -f "$APPS/peugeot300mille.desktop"
    for s in 32 48 64 128 256 512 1024; do rm -f "$ICONS/${s}x${s}/apps/peugeot300mille.png"; done
    gtk-update-icon-cache -q "$ICONS" 2>/dev/null || true
    update-desktop-database -q "$APPS" 2>/dev/null || true
    echo "peugeot300mille removed."
    exit 0
fi

cargo install --path . --locked
for s in 32 48 64 128 256 512 1024; do
    mkdir -p "$ICONS/${s}x${s}/apps"
    cp "assets/icon-$s.png" "$ICONS/${s}x${s}/apps/peugeot300mille.png"
done
mkdir -p "$APPS"
cat > "$APPS/peugeot300mille.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=peugeot300mille
Comment=Boîte de glitch vidéo analogique
Exec=$HOME/.cargo/bin/peugeot300mille %f
Icon=peugeot300mille
Terminal=false
Categories=AudioVideo;Video;
MimeType=video/mp4;video/quicktime;video/x-matroska;video/webm;video/x-msvideo;
StartupWMClass=peugeot300mille
DESKTOP
gtk-update-icon-cache -q "$ICONS" 2>/dev/null || true
update-desktop-database -q "$APPS" 2>/dev/null || true
echo "peugeot300mille installed: look for it in your applications, or run 'peugeot300mille'."
