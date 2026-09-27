#!/bin/sh
# Run as root during deployment; does not request a reboot.
set -eu

dramma_user=${1:-dramma}
case "$dramma_user" in
    ''|*[!a-zA-Z0-9_-]*) echo "Invalid kiosk username" >&2; exit 1 ;;
esac
id "$dramma_user" >/dev/null
test -x /usr/bin/systemctl
if ! command -v visudo >/dev/null 2>&1; then
    apt-get update -q
    apt-get install -y -q sudo
fi

mkdir -p /etc/sudoers.d
sudoers_file=$(mktemp /etc/sudoers.d/.dramma-reboot.XXXXXX)
trap 'rm -f "$sudoers_file"' EXIT
printf '%s ALL=(root) NOPASSWD: /usr/bin/systemctl --no-block reboot\n' "$dramma_user" > "$sudoers_file"
chown root:root "$sudoers_file"
chmod 0440 "$sudoers_file"
visudo -c -f "$sudoers_file"
mv "$sudoers_file" /etc/sudoers.d/dramma-reboot
