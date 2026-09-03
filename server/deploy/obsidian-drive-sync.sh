#!/bin/sh
set -eu
umask 077

inbox=/var/lib/obsidian-drive/inbox
config=/var/lib/obsidian-drive/.config/rclone/rclone.conf
remote='obsidian-drive:Obsidian — вложения (бот)'
log_file=/var/lib/obsidian-drive/sync.log

mkdir -p "$inbox"
chmod 700 /var/lib/obsidian-drive /var/lib/obsidian-drive/inbox

find "$inbox" -type f -name '*.part' -mmin +1440 -delete
find "$inbox" -mindepth 1 -depth -type d -empty -delete

exec 9>/var/lib/obsidian-drive/sync.lock
if ! flock -n 9; then
    exit 0
fi

if [ -f "$log_file" ] && [ "$(stat -c '%s' "$log_file")" -gt 5242880 ]; then
    : > "$log_file"
fi

if ! find "$inbox" -type f ! -name '*.part' -print -quit | grep -q .; then
    exit 0
fi

/usr/local/bin/rclone move "$inbox" "$remote" \
    --config "$config" \
    --exclude '*.part' \
    --delete-empty-src-dirs \
    --transfers 2 \
    --checkers 4 \
    --retries 3 \
    --low-level-retries 10 \
    --log-file "$log_file" \
    --log-level INFO
