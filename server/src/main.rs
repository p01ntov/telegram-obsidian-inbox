use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::{Query, State as AxumState};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use dotenvy::dotenv;
use flate2::read::GzDecoder;
use rand::Rng;
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::sleep;

const EVENT_VERSION: u8 = 1;
const PAIR_CODE_TTL_SECONDS: i64 = 600;
const MAX_EVENTS_PER_REQUEST: usize = 200;

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: i64,
    date: i64,
    chat: Chat,
    from: Option<User>,
    text: Option<String>,
    #[serde(default)]
    entities: Vec<MessageEntity>,
    caption: Option<String>,
    #[serde(default)]
    caption_entities: Vec<MessageEntity>,
    photo: Option<Vec<PhotoSize>>,
    document: Option<Document>,
    sticker: Option<Sticker>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PhotoSize {
    file_id: String,
    file_unique_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Document {
    file_id: String,
    file_unique_id: Option<String>,
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Sticker {
    file_id: String,
    file_unique_id: Option<String>,
    is_animated: bool,
    is_video: bool,
}

#[derive(Debug, Deserialize)]
struct MessageEntity {
    #[serde(rename = "type")]
    kind: String,
    offset: usize,
    length: usize,
    custom_emoji_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CustomEmojiSticker {
    file_id: String,
    is_animated: bool,
    is_video: bool,
    emoji: Option<String>,
    custom_emoji_id: Option<String>,
    needs_repainting: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiToken {
    token: String,
    device_name: String,
    created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairCode {
    code: String,
    expires_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PersistentState {
    offset: Option<i64>,
    #[serde(default)]
    processed: Vec<String>,
    #[serde(default)]
    api_tokens: Vec<ApiToken>,
    pair_code: Option<PairCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramEvent {
    version: u8,
    cursor: i64,
    id: String,
    day: String,
    markdown: String,
    assets: Vec<String>,
    message_id: i64,
    created_at: i64,
}

#[derive(Debug)]
struct Config {
    token: String,
    chat_id: i64,
    admin_user_id: i64,
    timezone: Tz,
    bind: SocketAddr,
    public_url: String,
    data_dir: PathBuf,
    vault_dir: PathBuf,
    events_dir: PathBuf,
    state_path: PathBuf,
    drive_inbox: Option<PathBuf>,
    retention_days: i64,
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    persistent: Arc<Mutex<PersistentState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentKind {
    Photo,
    Sticker,
    File,
}

#[derive(Debug)]
struct MediaFile {
    path: PathBuf,
    vault_path: String,
    kind: AttachmentKind,
    sticker_id: Option<String>,
}

#[derive(Debug)]
struct CustomEmojiAsset {
    path: PathBuf,
    vault_path: String,
    needs_repainting: bool,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EventsResponse {
    events: Vec<TelegramEvent>,
    latest_cursor: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    device_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairResponse {
    token: String,
    server_url: String,
}

#[derive(Debug, Deserialize)]
struct AssetQuery {
    path: String,
}

async fn api_call<T: DeserializeOwned>(
    client: &Client,
    token: &str,
    method: &str,
    params: Value,
) -> Result<T> {
    let endpoint = format!("https://api.telegram.org/bot{token}/{method}");
    let response = client
        .post(endpoint)
        .json(&params)
        .send()
        .await
        .map_err(|error| {
            anyhow!(
                "Telegram API request failed: {method}: {}",
                redact_transport_error(&error.to_string(), token)
            )
        })?;
    let status = response.status();
    let body = response.text().await?;
    let envelope: ApiEnvelope<T> = serde_json::from_str(&body)
        .with_context(|| format!("Invalid Telegram API response for {method}"))?;
    if !status.is_success() || !envelope.ok {
        let description = envelope.description.unwrap_or(body);
        let code = envelope
            .error_code
            .map(|value| format!(" ({value})"))
            .unwrap_or_default();
        bail!("Telegram API {method} error{code}: {description}");
    }
    envelope
        .result
        .ok_or_else(|| anyhow!("Telegram API {method} returned no result"))
}

fn redact_transport_error(message: &str, token: &str) -> String {
    message.replace(token, "[REDACTED]")
}

async fn send_message(client: &Client, config: &Config, chat_id: i64, text: &str) -> Result<()> {
    let _: Value = api_call(
        client,
        &config.token,
        "sendMessage",
        json!({"chat_id": chat_id, "text": text}),
    )
    .await?;
    Ok(())
}

async fn load_state(path: &Path) -> PersistentState {
    match fs::read_to_string(path).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => PersistentState::default(),
    }
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension("json.part");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?).await?;
    fs::rename(&temporary, path).await?;
    Ok(())
}

async fn save_shared_state(app: &AppState) -> Result<()> {
    let snapshot = app.persistent.lock().await.clone();
    write_json_atomic(&app.config.state_path, &snapshot).await
}

fn mark_processed(state: &mut PersistentState, key: String) {
    if !state.processed.iter().any(|item| item == &key) {
        state.processed.push(key);
    }
    if state.processed.len() > 5_000 {
        state.processed.drain(0..state.processed.len() - 5_000);
    }
}

fn random_hex(bytes: usize) -> String {
    let mut rng = rand::rng();
    (0..bytes)
        .map(|_| format!("{:02x}", rng.random::<u8>()))
        .collect()
}

fn generate_pair_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

fn is_authorized(headers: &HeaderMap, state: &PersistentState) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    state.api_tokens.iter().any(|known| known.token == token)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"ok": true, "service": "telegram-to-obsidian"}))
}

async fn pair_handler(
    AxumState(app): AxumState<AppState>,
    Json(request): Json<PairRequest>,
) -> Result<Json<PairResponse>, (StatusCode, Json<Value>)> {
    let now = Utc::now().timestamp();
    let mut state = app.persistent.lock().await;
    let valid = state
        .pair_code
        .as_ref()
        .map(|pair| pair.code == request.code.trim() && pair.expires_at >= now)
        .unwrap_or(false);
    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Код неверный или уже истёк"})),
        ));
    }
    state.pair_code = None;
    let token = random_hex(32);
    state.api_tokens.push(ApiToken {
        token: token.clone(),
        device_name: request
            .device_name
            .unwrap_or_else(|| "obsidian".to_string())
            .chars()
            .take(80)
            .collect(),
        created_at: now,
    });
    let snapshot = state.clone();
    drop(state);
    if let Err(error) = write_json_atomic(&app.config.state_path, &snapshot).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        ));
    }
    Ok(Json(PairResponse {
        token,
        server_url: app.config.public_url.clone(),
    }))
}

async fn events_handler(
    AxumState(app): AxumState<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, (StatusCode, Json<Value>)> {
    let authorized = {
        let persistent = app.persistent.lock().await;
        is_authorized(&headers, &persistent)
    };
    if !authorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        ));
    }
    let limit = query.limit.unwrap_or(100).clamp(1, MAX_EVENTS_PER_REQUEST);
    let mut entries = fs::read_dir(&app.config.events_dir)
        .await
        .map_err(internal_api_error)?;
    let mut events = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(internal_api_error)? {
        if !entry
            .file_type()
            .await
            .map_err(internal_api_error)?
            .is_file()
        {
            continue;
        }
        let Some(cursor) = entry
            .path()
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        if cursor <= query.after {
            continue;
        }
        let raw = fs::read(entry.path()).await.map_err(internal_api_error)?;
        let event: TelegramEvent = serde_json::from_slice(&raw).map_err(internal_api_error)?;
        events.push(event);
    }
    events.sort_by_key(|event| event.cursor);
    events.truncate(limit);
    let latest_cursor = events
        .last()
        .map(|event| event.cursor)
        .unwrap_or(query.after);
    Ok(Json(EventsResponse {
        events,
        latest_cursor,
    }))
}

async fn asset_handler(
    AxumState(app): AxumState<AppState>,
    headers: HeaderMap,
    Query(query): Query<AssetQuery>,
) -> Result<Response<Body>, (StatusCode, Json<Value>)> {
    let authorized = {
        let persistent = app.persistent.lock().await;
        is_authorized(&headers, &persistent)
    };
    if !authorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        ));
    }
    let relative = safe_relative_path(&query.path).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid path"})),
        )
    })?;
    let path = app.config.vault_dir.join(relative);
    let data = fs::read(&path).await.map_err(|error| {
        let status = if error.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(json!({"error": error.to_string()})))
    })?;
    let mime = mime_for_path(&path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "private, max-age=86400")
        .body(Body::from(data))
        .map_err(internal_api_error)
}

fn internal_api_error(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
}

fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() || value.contains('\0') || value.contains('\\') {
        return None;
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            _ => return None,
        }
    }
    if safe.as_os_str().is_empty() {
        None
    } else {
        Some(safe)
    }
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "application/json",
        "tgs" => "application/gzip",
        "webm" => "video/webm",
        "webp" => "image/webp",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "heic" | "heif" => "image/heic",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|character: char| character == ' ' || character == '.');
    let result: String = trimmed.chars().take(120).collect();
    if result.is_empty() {
        "файл".to_string()
    } else {
        result
    }
}

fn telegram_link(chat: &Chat, message_id: i64) -> Option<String> {
    if let Some(username) = &chat.username {
        return Some(format!("https://t.me/{username}/{message_id}"));
    }
    if chat.kind == "supergroup" {
        if let Some(internal_id) = chat.id.to_string().strip_prefix("-100") {
            return Some(format!("https://t.me/c/{internal_id}/{message_id}"));
        }
    }
    None
}

async fn download_telegram_file(
    client: &Client,
    config: &Config,
    file_id: &str,
    destination: &Path,
) -> Result<PathBuf> {
    if fs::metadata(destination).await.is_ok() {
        return Ok(destination.to_path_buf());
    }
    let file_info: TelegramFile = api_call(
        client,
        &config.token,
        "getFile",
        json!({"file_id": file_id}),
    )
    .await?;
    let remote_path = file_info
        .file_path
        .ok_or_else(|| anyhow!("Telegram returned no file path"))?;
    let response = client
        .get(format!(
            "https://api.telegram.org/file/bot{}/{}",
            config.token, remote_path
        ))
        .send()
        .await
        .map_err(|error| {
            anyhow!(
                "Telegram file download failed: {}",
                redact_transport_error(&error.to_string(), &config.token)
            )
        })?
        .error_for_status()?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temporary = destination.with_extension("download.part");
    let mut output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .await?;
    let mut response = response;
    while let Some(chunk) = response.chunk().await? {
        output.write_all(&chunk).await?;
    }
    output.flush().await?;
    drop(output);
    fs::rename(&temporary, destination).await?;
    Ok(destination.to_path_buf())
}

async fn find_attachment_by_unique_id(
    directory: &Path,
    unique_id: &str,
) -> Result<Option<PathBuf>> {
    let prefix = format!("telegram-{}.", safe_filename(unique_id));
    let mut entries = match fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file()
            && entry.file_name().to_string_lossy().starts_with(&prefix)
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn relative_to_vault(config: &Config, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(&config.vault_dir)
        .with_context(|| format!("{} is outside the server vault", path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

async fn fetch_media(
    client: &Client,
    config: &Config,
    message: &Message,
    day: &str,
) -> Result<Vec<MediaFile>> {
    let photo_directory = config.vault_dir.join("Универ/Вложения/фото");
    let sticker_directory = config.vault_dir.join("Универ/Вложения/стикеры");
    let file_directory = config.vault_dir.join("Универ/Вложения").join(day);
    let mut files = Vec::new();

    if let Some(largest) = message.photo.as_ref().and_then(|photos| photos.last()) {
        let path = if let Some(unique_id) = largest.file_unique_id.as_deref() {
            find_attachment_by_unique_id(&photo_directory, unique_id)
                .await?
                .unwrap_or_else(|| {
                    photo_directory.join(format!("telegram-{}.jpg", safe_filename(unique_id)))
                })
        } else {
            photo_directory.join(format!("telegram-message-{}.jpg", message.message_id))
        };
        let path = download_telegram_file(client, config, &largest.file_id, &path).await?;
        files.push(MediaFile {
            vault_path: relative_to_vault(config, &path)?,
            path,
            kind: AttachmentKind::Photo,
            sticker_id: None,
        });
    }

    if let Some(document) = &message.document {
        let original_name = document
            .file_name
            .as_deref()
            .map(safe_filename)
            .unwrap_or_else(|| "документ".to_string());
        let extension = Path::new(&original_name)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 12
                    && value
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
            })
            .unwrap_or_else(|| "bin".to_string());
        let is_photo = matches!(
            extension.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif"
        );
        let directory = if is_photo {
            &photo_directory
        } else {
            &file_directory
        };
        let path = if let Some(unique_id) = document.file_unique_id.as_deref() {
            find_attachment_by_unique_id(directory, unique_id)
                .await?
                .unwrap_or_else(|| {
                    directory.join(format!(
                        "telegram-{}.{}",
                        safe_filename(unique_id),
                        extension
                    ))
                })
        } else {
            directory.join(format!(
                "telegram-message-{}-{original_name}",
                message.message_id
            ))
        };
        let path = download_telegram_file(client, config, &document.file_id, &path).await?;
        files.push(MediaFile {
            vault_path: relative_to_vault(config, &path)?,
            path,
            kind: if is_photo {
                AttachmentKind::Photo
            } else {
                AttachmentKind::File
            },
            sticker_id: None,
        });
    }

    if let Some(sticker) = &message.sticker {
        let extension = if sticker.is_video {
            "webm"
        } else if sticker.is_animated {
            "tgs"
        } else {
            "webp"
        };
        let fallback_id = format!("message-{}", message.message_id);
        let sticker_id = sticker
            .file_unique_id
            .as_deref()
            .unwrap_or(&fallback_id)
            .to_string();
        let path = find_attachment_by_unique_id(&sticker_directory, &sticker_id)
            .await?
            .unwrap_or_else(|| {
                sticker_directory.join(format!(
                    "telegram-{}.{}",
                    safe_filename(&sticker_id),
                    extension
                ))
            });
        let path = download_telegram_file(client, config, &sticker.file_id, &path).await?;
        files.push(MediaFile {
            vault_path: relative_to_vault(config, &path)?,
            path,
            kind: AttachmentKind::Sticker,
            sticker_id: Some(sticker_id),
        });
    }
    Ok(files)
}

fn message_text_and_entities(message: &Message) -> (&str, &[MessageEntity]) {
    if let Some(text) = message.text.as_deref() {
        (text, &message.entities)
    } else if let Some(caption) = message.caption.as_deref() {
        (caption, &message.caption_entities)
    } else {
        ("", &[])
    }
}

fn utf16_boundary_to_byte(text: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let mut units = 0;
    for (byte_index, character) in text.char_indices() {
        if units == target {
            return Some(byte_index);
        }
        units += character.len_utf16();
    }
    (units == target).then_some(text.len())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn expand_tgs_json(tgs_path: &Path, json_path: &Path) -> Result<()> {
    if fs::metadata(json_path).await.is_ok() {
        return Ok(());
    }
    let compressed = fs::read(tgs_path).await?;
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut json_bytes = Vec::new();
    decoder.read_to_end(&mut json_bytes)?;
    let _: Value = serde_json::from_slice(&json_bytes)
        .context("Telegram .tgs did not contain valid Lottie JSON")?;
    fs::write(json_path, json_bytes).await?;
    Ok(())
}

async fn fetch_custom_emoji_assets(
    client: &Client,
    config: &Config,
    entities: &[MessageEntity],
) -> Result<HashMap<String, CustomEmojiAsset>> {
    let mut ids = Vec::new();
    for entity in entities {
        if entity.kind == "custom_emoji" {
            if let Some(id) = &entity.custom_emoji_id {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
    }
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let stickers: Vec<CustomEmojiSticker> = api_call(
        client,
        &config.token,
        "getCustomEmojiStickers",
        json!({"custom_emoji_ids": ids}),
    )
    .await?;
    let directory = config.vault_dir.join("Универ/Вложения/telegram-emoji");
    let mut assets = HashMap::new();
    for (index, sticker) in stickers.iter().enumerate() {
        let Some(id) = sticker
            .custom_emoji_id
            .clone()
            .or_else(|| ids.get(index).cloned())
        else {
            continue;
        };
        let extension = if sticker.is_video {
            "webm"
        } else if sticker.is_animated {
            "tgs"
        } else {
            "webp"
        };
        let downloaded = directory.join(format!("{id}.{extension}"));
        download_telegram_file(client, config, &sticker.file_id, &downloaded).await?;
        let path = if extension == "tgs" {
            let json_path = directory.join(format!("{id}.json"));
            expand_tgs_json(&downloaded, &json_path).await?;
            json_path
        } else {
            downloaded
        };
        let vault_path = relative_to_vault(config, &path)?;
        assets.insert(
            id.clone(),
            CustomEmojiAsset {
                path,
                vault_path,
                needs_repainting: sticker.needs_repainting.unwrap_or(false),
            },
        );
        println!(
            "Cached custom emoji {} ({})",
            id,
            sticker.emoji.as_deref().unwrap_or("fallback")
        );
    }
    Ok(assets)
}

fn custom_emoji_size(
    text: &str,
    entities: &[MessageEntity],
    assets: &HashMap<String, CustomEmojiAsset>,
) -> u32 {
    let count = entities
        .iter()
        .filter(|entity| entity.kind == "custom_emoji")
        .filter_map(|entity| entity.custom_emoji_id.as_deref())
        .filter(|id| assets.contains_key(*id))
        .count();
    if count == 0 {
        return 22;
    }
    let mut ranges = Vec::new();
    for entity in entities
        .iter()
        .filter(|entity| entity.kind == "custom_emoji")
    {
        let Some(id) = entity.custom_emoji_id.as_deref() else {
            continue;
        };
        if !assets.contains_key(id) {
            continue;
        }
        let Some(start) = utf16_boundary_to_byte(text, entity.offset) else {
            continue;
        };
        let Some(end) = utf16_boundary_to_byte(text, entity.offset + entity.length) else {
            continue;
        };
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    let mut cursor = 0;
    let mut has_text = false;
    for (start, end) in ranges {
        has_text |= text[cursor..start]
            .chars()
            .any(|character| !character.is_whitespace());
        cursor = end;
    }
    has_text |= text[cursor..]
        .chars()
        .any(|character| !character.is_whitespace());
    if has_text {
        22
    } else if count == 1 {
        96
    } else {
        ((112.0 / (count as f64).sqrt()).round() as u32).clamp(42, 84)
    }
}

fn render_custom_emoji(
    text: &str,
    entities: &[MessageEntity],
    assets: &HashMap<String, CustomEmojiAsset>,
    size: u32,
) -> String {
    let mut custom_entities: Vec<&MessageEntity> = entities
        .iter()
        .filter(|entity| entity.kind == "custom_emoji")
        .collect();
    custom_entities.sort_by_key(|entity| entity.offset);
    let mut result = String::with_capacity(text.len() + custom_entities.len() * 120);
    let mut cursor = 0;
    for entity in custom_entities {
        let Some(start) = utf16_boundary_to_byte(text, entity.offset) else {
            continue;
        };
        let Some(end) = utf16_boundary_to_byte(text, entity.offset + entity.length) else {
            continue;
        };
        if start < cursor || end > text.len() {
            continue;
        }
        let Some(id) = entity.custom_emoji_id.as_deref() else {
            continue;
        };
        let Some(asset) = assets.get(id) else {
            continue;
        };
        result.push_str(&text[cursor..start]);
        result.push_str(&format!(
            "<span class=\"tg-custom-emoji\" data-tg-id=\"{}\" data-tg-src=\"{}\" data-tg-size=\"{}\" data-tg-repaint=\"{}\">{}</span>",
            html_escape(id),
            html_escape(&asset.vault_path),
            size,
            if asset.needs_repainting { "1" } else { "0" },
            html_escape(&text[start..end])
        ));
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

fn media_note_line(media: &MediaFile) -> String {
    let extension = media
        .path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if media.kind == AttachmentKind::Sticker {
        if matches!(extension.as_str(), "tgs" | "webm") {
            return format!(
                "  <span class=\"tg-sticker\" data-tg-id=\"{}\" data-tg-src=\"{}\" data-tg-size=\"180\">🧩</span>",
                html_escape(media.sticker_id.as_deref().unwrap_or("telegram-sticker")),
                html_escape(&media.vault_path)
            );
        }
        return format!("  ![[{}|180]]", media.vault_path);
    }
    if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif"
    ) {
        format!("  ![[{}]]", media.vault_path)
    } else {
        format!("  [[{}]]", media.vault_path)
    }
}

async fn stage_for_drive(
    config: &Config,
    source: &Path,
    vault_path: &str,
    telegram_user_id: i64,
) -> Result<()> {
    let Some(inbox) = &config.drive_inbox else {
        return Ok(());
    };
    let relative = safe_relative_path(vault_path).ok_or_else(|| anyhow!("Unsafe vault path"))?;
    let parts: Vec<_> = relative.components().collect();
    let category = if vault_path.contains("/фото/") {
        PathBuf::from("фото")
    } else if vault_path.contains("/стикеры/") {
        PathBuf::from("стикеры")
    } else if vault_path.contains("/telegram-emoji/") {
        PathBuf::from("telegram-emoji")
    } else {
        parts
            .get(2)
            .map(|part| PathBuf::from(part.as_os_str()))
            .unwrap_or_else(|| PathBuf::from("файлы"))
    };
    let file_name = source
        .file_name()
        .ok_or_else(|| anyhow!("Attachment has no filename"))?;
    let destination = inbox
        .join("users")
        .join(telegram_user_id.to_string())
        .join(category)
        .join(file_name);
    if fs::metadata(&destination).await.is_ok() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temporary = destination.with_extension("upload.part");
    fs::copy(source, &temporary).await?;
    fs::rename(temporary, destination).await?;
    Ok(())
}

fn is_command(text: &str, command: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    first == command || first.starts_with(&format!("{command}@"))
}

async fn create_pair_code(client: &Client, app: &AppState, message: &Message) -> Result<()> {
    let code = generate_pair_code();
    let expires_at = Utc::now().timestamp() + PAIR_CODE_TTL_SECONDS;
    {
        let mut state = app.persistent.lock().await;
        state.pair_code = Some(PairCode {
            code: code.clone(),
            expires_at,
        });
    }
    save_shared_state(app).await?;
    send_message(
        client,
        &app.config,
        message.chat.id,
        &format!(
            "Панель Telegram → Obsidian\n\nСтатус: сервер работает\nAPI: {}\nКод привязки: {}\nКод одноразовый и действует 10 минут. Введи его в настройках плагина Telegram Custom Emoji.",
            app.config.public_url, code
        ),
    )
    .await
}

async fn process_message(client: &Client, app: &AppState, message: &Message) -> Result<()> {
    let key = format!("{}:{}", message.chat.id, message.message_id);
    if app.persistent.lock().await.processed.contains(&key) {
        return Ok(());
    }
    let (raw_text, entities) = message_text_and_entities(message);
    let is_admin = message
        .from
        .as_ref()
        .map(|user| user.id == app.config.admin_user_id)
        .unwrap_or(false);

    if is_command(raw_text.trim(), "/settings") {
        if is_admin {
            create_pair_code(client, app, message).await?;
        }
        let mut state = app.persistent.lock().await;
        mark_processed(&mut state, key);
        drop(state);
        save_shared_state(app).await?;
        return Ok(());
    }

    if !matches!(message.chat.kind.as_str(), "group" | "supergroup")
        || message.chat.id != app.config.chat_id
        || !is_admin
    {
        let mut state = app.persistent.lock().await;
        mark_processed(&mut state, key);
        drop(state);
        save_shared_state(app).await?;
        return Ok(());
    }

    let sent_at = DateTime::<Utc>::from_timestamp(message.date, 0)
        .unwrap_or_else(Utc::now)
        .with_timezone(&app.config.timezone);
    let day = sent_at.format("%Y-%m-%d").to_string();
    let media = fetch_media(client, &app.config, message, &day).await?;
    let emoji_assets = fetch_custom_emoji_assets(client, &app.config, entities).await?;
    let size = custom_emoji_size(raw_text, entities, &emoji_assets);
    let rendered_text = render_custom_emoji(raw_text, entities, &emoji_assets, size);
    let compact_text = rendered_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    let message_body = if compact_text.is_empty() && media.is_empty() {
        "сообщение без текста или вложения".to_string()
    } else if compact_text.is_empty() {
        "вложение".to_string()
    } else {
        compact_text
    };
    let mut line = format!("- {} · {}", sent_at.format("%H:%M"), message_body);
    if let Some(link) = telegram_link(&message.chat, message.message_id) {
        line.push_str(&format!(" [↗]({link})"));
    }
    let mut body = vec![format!("<!-- tg-event:{key} -->"), line];
    for item in &media {
        body.push(media_note_line(item));
    }
    body.push(format!("<!-- /tg-event:{key} -->"));

    let mut assets: Vec<String> = media.iter().map(|item| item.vault_path.clone()).collect();
    assets.extend(emoji_assets.values().map(|asset| asset.vault_path.clone()));
    assets.sort();
    assets.dedup();

    let telegram_user_id = message
        .from
        .as_ref()
        .map(|user| user.id)
        .unwrap_or(app.config.admin_user_id);
    for item in &media {
        stage_for_drive(&app.config, &item.path, &item.vault_path, telegram_user_id).await?;
    }
    for item in emoji_assets.values() {
        stage_for_drive(&app.config, &item.path, &item.vault_path, telegram_user_id).await?;
    }

    let event = TelegramEvent {
        version: EVENT_VERSION,
        cursor: message.message_id,
        id: key.clone(),
        day,
        markdown: body.join("\n"),
        assets,
        message_id: message.message_id,
        created_at: Utc::now().timestamp(),
    };
    write_json_atomic(
        &app.config
            .events_dir
            .join(format!("{}.json", message.message_id)),
        &event,
    )
    .await?;
    {
        let mut state = app.persistent.lock().await;
        mark_processed(&mut state, key);
    }
    save_shared_state(app).await?;
    println!("Queued Telegram event {}", event.id);
    Ok(())
}

async fn telegram_loop(client: Client, app: AppState) -> Result<()> {
    let bot: User = api_call(&client, &app.config.token, "getMe", json!({})).await?;
    println!(
        "Bot @{} is running server-side",
        bot.username
            .as_deref()
            .or(bot.first_name.as_deref())
            .unwrap_or("unknown")
    );
    loop {
        let offset = app.persistent.lock().await.offset;
        let mut params = json!({"timeout": 25, "allowed_updates": ["message"]});
        if let Some(offset) = offset {
            params["offset"] = json!(offset);
        }
        let updates: Vec<Update> =
            match api_call(&client, &app.config.token, "getUpdates", params).await {
                Ok(updates) => updates,
                Err(error) => {
                    eprintln!("{error}; retrying in 5 seconds");
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
        for update in updates {
            if let Some(message) = update.message {
                if let Err(error) = process_message(&client, &app, &message).await {
                    eprintln!("Could not process update {}: {error:#}", update.update_id);
                    sleep(Duration::from_secs(5)).await;
                    break;
                }
            }
            {
                let mut state = app.persistent.lock().await;
                state.offset = Some(update.update_id + 1);
            }
            save_shared_state(&app).await?;
        }
    }
}

async fn cleanup_loop(app: AppState) {
    loop {
        if let Err(error) = cleanup_old_files(&app.config).await {
            eprintln!("Cleanup failed: {error:#}");
        }
        sleep(Duration::from_secs(3600)).await;
    }
}

async fn cleanup_old_files(config: &Config) -> Result<()> {
    if config.retention_days <= 0 {
        return Ok(());
    }
    let cutoff = Utc::now().timestamp() - config.retention_days * 86_400;
    cleanup_tree(&config.events_dir, cutoff).await?;
    cleanup_tree(&config.vault_dir, cutoff).await?;
    Ok(())
}

async fn cleanup_tree(root: &Path, cutoff: i64) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if !file_type.is_file() || entry.file_name().to_string_lossy().ends_with(".part") {
                continue;
            }
            let modified = entry
                .metadata()
                .await?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            if modified < cutoff {
                let _ = fs::remove_file(entry.path()).await;
            }
        }
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String> {
    let value = env::var(name).unwrap_or_default().trim().to_string();
    if value.is_empty() {
        bail!("{name} is empty");
    }
    Ok(value)
}

fn load_config() -> Result<Config> {
    let data_dir = PathBuf::from(
        env::var("SERVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/telegram-obsidian".to_string()),
    );
    let timezone_name =
        env::var("OBSIDIAN_TIMEZONE").unwrap_or_else(|_| "Europe/Moscow".to_string());
    let public_url = required_env("PUBLIC_API_URL")?
        .trim_end_matches('/')
        .to_string();
    Ok(Config {
        token: required_env("BOT_TOKEN")?,
        chat_id: required_env("TELEGRAM_CHAT_ID")?
            .parse()
            .context("Invalid TELEGRAM_CHAT_ID")?,
        admin_user_id: required_env("ADMIN_USER_ID")?
            .parse()
            .context("Invalid ADMIN_USER_ID")?,
        timezone: timezone_name
            .parse()
            .with_context(|| format!("Unknown timezone: {timezone_name}"))?,
        bind: env::var("SERVER_BIND")
            .unwrap_or_else(|_| "127.0.0.1:18765".to_string())
            .parse()
            .context("Invalid SERVER_BIND")?,
        public_url,
        vault_dir: data_dir.join("vault"),
        events_dir: data_dir.join("events"),
        state_path: data_dir.join("state.json"),
        drive_inbox: env::var("DRIVE_INBOX")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        retention_days: env::var("SERVER_RETENTION_DAYS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .context("Invalid SERVER_RETENTION_DAYS")?,
        data_dir,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let config = Arc::new(load_config()?);
    fs::create_dir_all(&config.data_dir).await?;
    fs::create_dir_all(&config.vault_dir).await?;
    fs::create_dir_all(&config.events_dir).await?;
    if let Some(inbox) = &config.drive_inbox {
        fs::create_dir_all(inbox).await?;
    }
    let persistent = load_state(&config.state_path).await;
    let app = AppState {
        config: config.clone(),
        persistent: Arc::new(Mutex::new(persistent)),
    };
    let router = Router::new()
        .route("/v1/health", get(health_handler))
        .route("/v1/pair", post(pair_handler))
        .route("/v1/events", get(events_handler))
        .route("/v1/asset", get(asset_handler))
        .with_state(app.clone());
    let listener = TcpListener::bind(config.bind).await?;
    println!("API listening on {}", config.bind);

    let client = Client::builder()
        .user_agent("telegram-to-obsidian-server/0.2")
        .build()?;
    tokio::spawn(cleanup_loop(app.clone()));
    tokio::try_join!(
        async {
            axum::serve(listener, router)
                .await
                .map_err(anyhow::Error::from)
        },
        telegram_loop(client, app),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_asset_paths() {
        assert!(safe_relative_path("Универ/Вложения/фото/a.jpg").is_some());
        assert!(safe_relative_path("../etc/passwd").is_none());
        assert!(safe_relative_path("/etc/passwd").is_none());
        assert!(safe_relative_path("folder\\file").is_none());
    }

    #[test]
    fn telegram_utf16_offsets_are_converted() {
        let text = "a😀b";
        assert_eq!(utf16_boundary_to_byte(text, 0), Some(0));
        assert_eq!(utf16_boundary_to_byte(text, 1), Some(1));
        assert_eq!(utf16_boundary_to_byte(text, 3), Some(5));
        assert_eq!(utf16_boundary_to_byte(text, 4), Some(6));
    }
}
