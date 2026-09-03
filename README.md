# Telegram Inbox & Custom Emoji for Obsidian

An Obsidian plugin and optional self-hosted Rust service for importing a private Telegram chat into daily notes.

It works on desktop, iOS, and Android and renders Telegram premium emoji and stickers in WebP, WebM, TGS, and Lottie JSON formats.

## Features

- Imports text, photos, files, stickers, and premium emoji.
- Writes regular Markdown and attachment files into the vault.
- Prevents duplicate events by their stable Telegram message links and does not add visible technical markers to notes.
- Scales emoji like Telegram: compact next to text and larger for emoji-only messages.
- Keeps API credentials and the device name local instead of syncing them in plugin `data.json`.
- Works alongside Self-hosted LiveSync; LiveSync only sees ordinary vault files.
- Optional private Google Drive staging uploader with a 30-second systemd timer.

## Install with BRAT

1. Install **BRAT** from Obsidian Community Plugins.
2. Open `Settings → BRAT → Add Beta plugin`.
3. Paste this repository URL.
4. Enable **Telegram Custom Emoji**.
5. Enter the HTTPS URL of your server in the plugin settings.
6. Send `/settings` to your Telegram bot from the configured administrator account.
7. Enter the one-use six-digit pairing code and choose a unique device name such as `iphone`.

BRAT installs the release assets `main.js`, `manifest.json`, and `styles.css`.

## Build the plugin

```bash
npm ci
npm run build
```

Copy `dist/main.js` to `main.js` before creating a release.

## Run the server

The Rust service uses Telegram Bot API long polling and exposes a small authenticated HTTPS API for Obsidian clients.

```bash
cd server
cargo build --release
```

Required environment variables:

```dotenv
BOT_TOKEN=replace_me
TELEGRAM_CHAT_ID=-1000000000000
ADMIN_USER_ID=123456789
PUBLIC_API_URL=https://notes.example.com/obsidian-inbox
```

See [`server/server.env.example`](server/server.env.example) and [`server/deploy`](server/deploy) for systemd and nginx examples. Put TLS in front of the local API; iOS requires HTTPS.

The `/settings` command only works for `ADMIN_USER_ID`. Pairing codes are one-use and expire after ten minutes. Each device receives its own bearer token.

## Vault layout

```text
Универ/
  YYYY-MM-DD.md
  Вложения/
    фото/
    стикеры/
    telegram-emoji/
    YYYY-MM-DD/
```

## Privacy

The plugin never makes the vault or attachments public. Your server URL is configured locally. Telegram bot credentials stay on the server, and device API tokens stay in device-local storage.

## License

MIT

## Changes in 0.6.3

- Removed `tg-event` HTML comments from generated notes.
- Existing legacy marker lines are removed automatically when the plugin starts.
