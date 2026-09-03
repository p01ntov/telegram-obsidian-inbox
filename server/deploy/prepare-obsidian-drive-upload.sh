#!/bin/sh
set -eu

key_file=/tmp/obsidian-upload.pub
authorized_keys=/var/lib/obsidian-drive/.ssh/authorized_keys

test -s "$key_file"
install -d -o obsidian-drive -g obsidian-drive -m 700 /var/lib/obsidian-drive/.ssh

key=$(tr -d '\r\n' < "$key_file")
line="restrict,command=\"internal-sftp\" $key"

if [ -f "$authorized_keys" ]; then
    if ! grep -Fqx -- "$line" "$authorized_keys"; then
        printf '%s\n' "$line" >> "$authorized_keys"
    fi
else
    printf '%s\n' "$line" > "$authorized_keys"
fi

chown obsidian-drive:obsidian-drive "$authorized_keys"
chmod 600 "$authorized_keys"
install -d -o obsidian-drive -g obsidian-drive -m 700 /var/lib/obsidian-drive/inbox
rm -f "$key_file"

stat -c '%U %a %n' "$authorized_keys" /var/lib/obsidian-drive/inbox
