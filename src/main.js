const {
  Plugin,
  Notice,
  PluginSettingTab,
  Setting,
  requestUrl,
  normalizePath,
  Platform,
} = require("obsidian");
const { RangeSetBuilder } = require("@codemirror/state");
const { Decoration, ViewPlugin, WidgetType } = require("@codemirror/view");
const { gunzipSync } = require("fflate");

const PLUGIN_NAME = "Telegram Custom Emoji";
const DEFAULT_SETTINGS = {
  serverUrl: "",
  apiToken: "",
  deviceName: "",
  autoSync: true,
  pollIntervalSeconds: 30,
};
const TELEGRAM_EMOJI_PATTERN =
  /<span\b(?=[^>]*\bclass=["']tg-(?:custom-emoji|sticker)["'])(?=[^>]*\bdata-tg-id=["']([^"']+)["'])(?=[^>]*\bdata-tg-src=["']([^"']+)["'])[^>]*>([\s\S]*?)<\/span>/g;

function decodeHtml(value) {
  const element = document.createElement("textarea");
  element.innerHTML = value;
  return element.value;
}

function defaultDeviceName() {
  if (Platform.isIosApp) return "iphone";
  if (Platform.isAndroidApp) return "android";
  return "desktop";
}

function cleanServerUrl(value) {
  return String(value || "").trim().replace(/\/+$/, "");
}

function safeVaultPath(value) {
  const raw = String(value || "").replace(/\\/g, "/");
  if (!raw || raw.startsWith("/") || raw.split("/").includes("..")) {
    throw new Error(`Небезопасный путь от сервера: ${raw}`);
  }
  return normalizePath(raw);
}

function dayTemplate(day) {
  return `---
type: university-day
date: ${day}
tags:
  - универ
  - daily
---

# Универ — ${day}

## Записи дня

## Задачи

## Скрины и файлы

## Лента Telegram

## Итог дня
`;
}

function insertTelegramBlock(content, event) {
  const marker = `<!-- tg-event:${event.id} -->`;
  if (content.includes(marker)) return content;

  const heading = "## Лента Telegram";
  let result = content;
  if (!result.includes(heading)) {
    result = `${result.trimEnd()}\n\n${heading}\n\n`;
  }
  const headingIndex = result.indexOf(heading);
  const sectionStart = headingIndex + heading.length;
  const nextHeadingMatch = /\n##\s+/g;
  nextHeadingMatch.lastIndex = sectionStart;
  const next = nextHeadingMatch.exec(result);
  const sectionEnd = next ? next.index : result.length;
  const before = result.slice(0, sectionEnd).trimEnd();
  const after = result.slice(sectionEnd).replace(/^\n+/, "");
  return `${before}\n\n${event.markdown.trim()}\n\n${after}`.trimEnd() + "\n";
}

class TelegramEmojiWidget extends WidgetType {
  constructor(plugin, id, sourcePath, fallback, size) {
    super();
    this.plugin = plugin;
    this.id = id;
    this.sourcePath = sourcePath;
    this.fallback = fallback || "🙂";
    this.size = size || 22;
  }

  eq(other) {
    return (
      other instanceof TelegramEmojiWidget &&
      other.id === this.id &&
      other.sourcePath === this.sourcePath &&
      other.fallback === this.fallback &&
      other.size === this.size
    );
  }

  toDOM() {
    const host = document.createElement("span");
    host.className = "tg-custom-emoji-host";
    host.dataset.tgId = this.id;
    host.dataset.tgSrc = this.sourcePath;
    host.dataset.tgSize = String(this.size);
    host.textContent = this.fallback;
    void this.plugin.renderAsset(host, this.sourcePath, this.fallback, this.size);
    return host;
  }

  ignoreEvent() {
    return true;
  }
}

function createEditorExtension(plugin) {
  class TelegramEmojiViewPlugin {
    constructor(view) {
      this.decorations = this.buildDecorations(view);
    }

    update(update) {
      if (update.docChanged || update.viewportChanged) {
        this.decorations = this.buildDecorations(update.view);
      }
    }

    buildDecorations(view) {
      const builder = new RangeSetBuilder();
      const source = view.state.doc.toString();
      TELEGRAM_EMOJI_PATTERN.lastIndex = 0;
      let match;
      while ((match = TELEGRAM_EMOJI_PATTERN.exec(source)) !== null) {
        const start = match.index;
        const end = start + match[0].length;
        const attributes = match[0].slice(0, match[0].indexOf(">"));
        const sizeMatch = attributes.match(/\bdata-tg-size=["'](\d+)["']/);
        const size = sizeMatch ? Number(sizeMatch[1]) : 22;
        builder.add(
          start,
          end,
          Decoration.replace({
            widget: new TelegramEmojiWidget(
              plugin,
              match[1],
              match[2],
              decodeHtml(match[3]),
              size,
            ),
          }),
        );
      }
      return builder.finish();
    }
  }

  return ViewPlugin.fromClass(TelegramEmojiViewPlugin, {
    decorations: (value) => value.decorations,
  });
}

class TelegramSyncSettingTab extends PluginSettingTab {
  constructor(app, plugin) {
    super(app, plugin);
    this.plugin = plugin;
    this.pairCode = "";
  }

  display() {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl("h2", { text: "Telegram → Obsidian" });
    containerEl.createEl("p", {
      text: "Бот работает на VPS. Плагин забирает сообщения и вложения по HTTPS; Self-hosted LiveSync vrtmrz затем синхронизирует обычные файлы vault между устройствами.",
    });

    new Setting(containerEl)
      .setName("Адрес сервера")
      .setDesc("HTTPS-адрес серверной очереди")
      .addText((text) =>
        text
          .setPlaceholder("https://your-server.example/obsidian-inbox")
          .setValue(this.plugin.settings.serverUrl)
          .onChange(async (value) => {
            this.plugin.settings.serverUrl = cleanServerUrl(value);
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Имя устройства")
      .setDesc("Например: desktop, iphone или android")
      .addText((text) =>
        text
          .setPlaceholder(defaultDeviceName())
          .setValue(this.plugin.settings.deviceName)
          .onChange(async (value) => {
            this.plugin.settings.deviceName = value.trim();
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Одноразовый код")
      .setDesc("Отправь /settings в Telegram-группу и введи шестизначный код")
      .addText((text) =>
        text.setPlaceholder("000000").onChange((value) => {
          this.pairCode = value.trim();
        }),
      )
      .addButton((button) =>
        button.setButtonText("Привязать").setCta().onClick(async () => {
          button.setDisabled(true);
          try {
            await this.plugin.pair(this.pairCode);
            new Notice("Устройство привязано к серверному боту");
            this.display();
          } catch (error) {
            new Notice(`Привязка не удалась: ${error.message || error}`);
          } finally {
            button.setDisabled(false);
          }
        }),
      );

    new Setting(containerEl)
      .setName("Статус авторизации")
      .setDesc(
        this.plugin.settings.apiToken
          ? "Устройство привязано. Токен скрыт."
          : "Не привязано — получи код командой /settings.",
      )
      .addButton((button) =>
        button.setButtonText("Проверить").onClick(async () => {
          button.setDisabled(true);
          try {
            await this.plugin.checkServer();
            new Notice("Сервер доступен");
          } catch (error) {
            new Notice(`Сервер недоступен: ${error.message || error}`);
          } finally {
            button.setDisabled(false);
          }
        }),
      );

    new Setting(containerEl)
      .setName("Автосинхронизация Telegram")
      .setDesc("Проверять очередь, пока Obsidian открыт")
      .addToggle((toggle) =>
        toggle.setValue(this.plugin.settings.autoSync).onChange(async (value) => {
          this.plugin.settings.autoSync = value;
          await this.plugin.saveSettings();
          this.plugin.configurePolling();
        }),
      );

    new Setting(containerEl)
      .setName("Интервал проверки")
      .setDesc("Секунды; минимум 15")
      .addText((text) =>
        text
          .setValue(String(this.plugin.settings.pollIntervalSeconds))
          .onChange(async (value) => {
            const parsed = Number.parseInt(value, 10);
            if (Number.isFinite(parsed)) {
              this.plugin.settings.pollIntervalSeconds = Math.max(15, parsed);
              await this.plugin.saveSettings();
              this.plugin.configurePolling();
            }
          }),
      );

    new Setting(containerEl)
      .setName("Забрать сообщения сейчас")
      .setDesc(this.plugin.lastSyncSummary || "Ручная проверка серверной очереди")
      .addButton((button) =>
        button.setButtonText("Синхронизировать").setCta().onClick(async () => {
          button.setDisabled(true);
          try {
            await this.plugin.syncNow(true);
            this.display();
          } finally {
            button.setDisabled(false);
          }
        }),
      );
  }
}

class TelegramCustomEmojiPlugin extends Plugin {
  async onload() {
    const persistedSettings = (await this.loadData()) || {};
    const localDeviceSettings = this.loadLocalDeviceSettings();
    this.settings = Object.assign({}, DEFAULT_SETTINGS, persistedSettings);
    this.settings.serverUrl = cleanServerUrl(this.settings.serverUrl);
    this.settings.apiToken =
      localDeviceSettings.apiToken || persistedSettings.apiToken || "";
    this.settings.deviceName =
      localDeviceSettings.deviceName ||
      persistedSettings.deviceName ||
      defaultDeviceName();
    this.objectUrls = new Set();
    this.animations = new Set();
    this.syncPromise = null;
    this.pollTimer = null;
    this.lastSyncSummary = "";

    // Migrate old installations away from syncable data.json. API credentials
    // and the device identity must stay local to this Obsidian installation.
    if (persistedSettings.apiToken || persistedSettings.deviceName) {
      await this.saveSettings();
    }

    this.addSettingTab(new TelegramSyncSettingTab(this.app, this));
    this.registerMarkdownPostProcessor(async (element) => {
      const nodes = Array.from(
        element.querySelectorAll(".tg-custom-emoji, .tg-sticker"),
      );
      await Promise.all(nodes.map((node) => this.renderNode(node)));
    });
    this.registerEditorExtension(createEditorExtension(this));

    this.domObserver = new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const addedNode of mutation.addedNodes) this.scanDomNode(addedNode);
      }
    });
    if (document.body) {
      this.domObserver.observe(document.body, { childList: true, subtree: true });
      this.scanDomNode(document.body);
    }

    this.addCommand({
      id: "telegram-sync-now",
      name: "Синхронизировать Telegram сейчас",
      callback: () => void this.syncNow(true),
    });
    this.addCommand({
      id: "telegram-custom-emoji-reload",
      name: "Перезагрузить Telegram-эмодзи",
      callback: () => {
        this.rerenderMarkdown();
        new Notice("Telegram-эмодзи перезагружены");
      },
    });

    this.app.workspace.onLayoutReady(() => {
      this.configurePolling();
      window.setTimeout(() => {
        this.rerenderMarkdown();
        if (this.settings.autoSync && this.settings.apiToken) void this.syncNow(false);
      }, 1200);
    });
    console.log(`${PLUGIN_NAME} loaded on ${this.settings.deviceName}`);
  }

  onunload() {
    this.domObserver?.disconnect();
    if (this.pollTimer !== null) window.clearInterval(this.pollTimer);
    for (const animation of this.animations) {
      try {
        animation.destroy();
      } catch (_) {}
    }
    for (const url of this.objectUrls) URL.revokeObjectURL(url);
    this.animations.clear();
    this.objectUrls.clear();
  }

  localDeviceStorageKey() {
    return `telegram-custom-emoji:device:${this.app.vault.getName()}`;
  }

  loadLocalDeviceSettings() {
    try {
      const value = window.localStorage.getItem(this.localDeviceStorageKey());
      if (!value) return {};
      const parsed = JSON.parse(value);
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch (_) {
      return {};
    }
  }

  saveLocalDeviceSettings() {
    window.localStorage.setItem(
      this.localDeviceStorageKey(),
      JSON.stringify({
        apiToken: this.settings.apiToken || "",
        deviceName: this.settings.deviceName || defaultDeviceName(),
      }),
    );
  }

  async saveSettings() {
    this.saveLocalDeviceSettings();
    const {
      apiToken: _localApiToken,
      deviceName: _localDeviceName,
      ...syncableSettings
    } = this.settings;
    await this.saveData(syncableSettings);
  }

  configurePolling() {
    if (this.pollTimer !== null) {
      window.clearInterval(this.pollTimer);
      this.pollTimer = null;
    }
    if (!this.settings.autoSync) return;
    const seconds = Math.max(15, Number(this.settings.pollIntervalSeconds) || 30);
    this.pollTimer = window.setInterval(() => void this.syncNow(false), seconds * 1000);
    this.registerInterval(this.pollTimer);
  }

  apiUrl(path) {
    const base = cleanServerUrl(this.settings.serverUrl);
    if (!base.startsWith("https://")) throw new Error("Для iOS нужен HTTPS-адрес сервера");
    return `${base}${path}`;
  }

  authHeaders() {
    return {
      Authorization: `Bearer ${this.settings.apiToken}`,
      Accept: "application/json",
    };
  }

  async pair(code) {
    if (!/^\d{6}$/.test(code || "")) {
      throw new Error("введи шестизначный код из /settings");
    }
    const response = await requestUrl({
      url: this.apiUrl("/v1/pair"),
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({
        code,
        deviceName: this.settings.deviceName || defaultDeviceName(),
      }),
      throw: false,
    });
    if (response.status !== 200 || !response.json?.token) {
      throw new Error(response.json?.error || `HTTP ${response.status}`);
    }
    this.settings.apiToken = response.json.token;
    if (response.json.serverUrl) this.settings.serverUrl = cleanServerUrl(response.json.serverUrl);
    await this.saveSettings();
    await this.syncNow(false);
  }

  async checkServer() {
    const response = await requestUrl({
      url: this.apiUrl("/v1/health"),
      method: "GET",
      throw: false,
    });
    if (response.status !== 200 || !response.json?.ok) throw new Error(`HTTP ${response.status}`);
  }

  cursorStorageKey() {
    return `telegram-custom-emoji:cursor:${this.app.vault.getName()}:${this.settings.deviceName || defaultDeviceName()}`;
  }

  getCursor() {
    const parsed = Number.parseInt(window.localStorage.getItem(this.cursorStorageKey()) || "0", 10);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
  }

  setCursor(cursor) {
    window.localStorage.setItem(this.cursorStorageKey(), String(cursor));
  }

  async syncNow(showNotice = false) {
    if (this.syncPromise) return this.syncPromise;
    this.syncPromise = this.performSync(showNotice).finally(() => {
      this.syncPromise = null;
    });
    return this.syncPromise;
  }

  async performSync(showNotice) {
    if (!this.settings.apiToken) {
      if (showNotice) new Notice("Сначала привяжи плагин кодом из /settings");
      return 0;
    }
    let cursor = this.getCursor();
    let imported = 0;
    try {
      while (true) {
        const response = await requestUrl({
          url: this.apiUrl(`/v1/events?after=${cursor}&limit=100`),
          method: "GET",
          headers: this.authHeaders(),
          throw: false,
        });
        if (response.status === 401) {
          throw new Error("сервер отклонил токен — выполни /settings и привяжи заново");
        }
        if (response.status !== 200 || !Array.isArray(response.json?.events)) {
          throw new Error(response.json?.error || `HTTP ${response.status}`);
        }
        const events = response.json.events;
        if (events.length === 0) break;
        for (const event of events) {
          await this.importEvent(event);
          cursor = Math.max(cursor, Number(event.cursor) || 0);
          this.setCursor(cursor);
          imported += 1;
        }
        if (events.length < 100) break;
      }
      this.lastSyncSummary = imported ? `Добавлено сообщений: ${imported}` : "Новых сообщений нет";
      if (showNotice) new Notice(this.lastSyncSummary);
      if (imported) this.rerenderMarkdown();
      return imported;
    } catch (error) {
      this.lastSyncSummary = `Ошибка: ${error.message || error}`;
      console.warn(`${PLUGIN_NAME}: sync failed`, error);
      if (showNotice) new Notice(`Telegram Sync: ${error.message || error}`);
      return 0;
    }
  }

  async importEvent(event) {
    if (!event || event.version !== 1 || !/^\d{4}-\d{2}-\d{2}$/.test(event.day)) {
      throw new Error("сервер вернул неподдерживаемое событие");
    }
    for (const asset of event.assets || []) await this.downloadAsset(asset);
    const notePath = normalizePath(`Универ/${event.day}.md`);
    await this.ensureParent(notePath);
    const existing = this.app.vault.getAbstractFileByPath(notePath);
    if (existing && typeof this.app.vault.process === "function") {
      await this.app.vault.process(existing, (content) => insertTelegramBlock(content, event));
      return;
    }
    const adapter = this.app.vault.adapter;
    const content = (await adapter.exists(notePath)) ? await adapter.read(notePath) : dayTemplate(event.day);
    const updated = insertTelegramBlock(content, event);
    if (updated !== content) await adapter.write(notePath, updated);
  }

  async downloadAsset(rawPath) {
    const path = safeVaultPath(rawPath);
    const adapter = this.app.vault.adapter;
    if (await adapter.exists(path)) return;
    await this.ensureParent(path);
    const response = await requestUrl({
      url: this.apiUrl(`/v1/asset?path=${encodeURIComponent(path)}`),
      method: "GET",
      headers: this.authHeaders(),
      throw: false,
    });
    if (response.status !== 200 || !response.arrayBuffer) {
      throw new Error(`не удалось скачать ${path}: HTTP ${response.status}`);
    }
    await adapter.writeBinary(path, response.arrayBuffer);
  }

  async ensureParent(path) {
    const parts = normalizePath(path).split("/").slice(0, -1);
    let current = "";
    for (const part of parts) {
      current = current ? `${current}/${part}` : part;
      if (!(await this.app.vault.adapter.exists(current))) await this.app.vault.adapter.mkdir(current);
    }
  }

  rerenderMarkdown() {
    this.app.workspace.iterateAllLeaves((leaf) => {
      if (leaf.view?.getViewType?.() === "markdown") leaf.view.previewMode?.rerender?.(true);
    });
  }

  scanDomNode(node) {
    if (!node || node.nodeType !== 1 || typeof node.matches !== "function") return;
    const nodes = [];
    if (node.matches(".tg-custom-emoji, .tg-sticker")) nodes.push(node);
    nodes.push(...node.querySelectorAll(".tg-custom-emoji, .tg-sticker"));
    for (const emojiNode of nodes) void this.renderNode(emojiNode);
  }

  async renderNode(node) {
    if (node.dataset.tgRendered === "1") return;
    node.dataset.tgRendered = "1";
    await this.renderAsset(
      node,
      node.dataset.tgSrc,
      node.textContent || "🙂",
      Number(node.dataset.tgSize || 22),
    );
  }

  async renderAsset(node, sourcePath, fallback, size = 22) {
    if (!sourcePath) {
      this.restoreFallback(node, fallback);
      return;
    }
    try {
      const data = await this.app.vault.adapter.readBinary(safeVaultPath(sourcePath));
      const extension = sourcePath.split(".").pop().toLowerCase();
      this.setSize(node, size);
      node.classList.add("tg-custom-emoji-host");
      node.classList.remove("tg-custom-emoji-fallback");
      node.replaceChildren();
      if (extension === "tgs") {
        this.renderLottie(node, this.decodeTgs(data));
      } else if (extension === "json") {
        this.renderLottie(node, JSON.parse(new TextDecoder().decode(new Uint8Array(data))));
      } else if (extension === "webm") {
        this.renderWebm(node, data);
      } else {
        this.renderImage(node, data, extension);
      }
    } catch (error) {
      console.warn(`${PLUGIN_NAME}: ${error.message || error}`);
      this.restoreFallback(node, fallback);
    }
  }

  decodeTgs(data) {
    return JSON.parse(new TextDecoder().decode(gunzipSync(new Uint8Array(data))));
  }

  renderLottie(node, animationData) {
    const container = document.createElement("span");
    container.className = "tg-custom-emoji-lottie";
    node.appendChild(container);
    const animation = this.getLottie().loadAnimation({
      container,
      renderer: "svg",
      loop: true,
      autoplay: true,
      animationData,
      rendererSettings: { progressiveLoad: true },
    });
    this.animations.add(animation);
  }

  getLottie() {
    if (this.lottieRuntime) return this.lottieRuntime;
    const loaded = require("lottie-web/build/player/lottie_light.min.js");
    const runtime = loaded.default || loaded;
    if (!runtime || typeof runtime.loadAnimation !== "function") throw new Error("Lottie runtime недоступен");
    this.lottieRuntime = runtime;
    return runtime;
  }

  renderWebm(node, data) {
    const url = this.makeObjectUrl(data, "video/webm");
    const video = document.createElement("video");
    video.className = "tg-custom-emoji-media";
    video.autoplay = true;
    video.loop = true;
    video.muted = true;
    video.playsInline = true;
    video.preload = "auto";
    video.setAttribute("playsinline", "");
    video.setAttribute("webkit-playsinline", "");
    video.setAttribute("aria-label", node.dataset.tgId || "Telegram custom emoji");
    video.src = url;
    node.appendChild(video);
    void video.play().catch(() => {});
  }

  renderImage(node, data, extension) {
    const mime = {
      png: "image/png",
      jpg: "image/jpeg",
      jpeg: "image/jpeg",
      gif: "image/gif",
      heic: "image/heic",
      heif: "image/heic",
    }[extension] || "image/webp";
    const url = this.makeObjectUrl(data, mime);
    const image = document.createElement("img");
    image.className = "tg-custom-emoji-media";
    image.alt = node.dataset.tgId || "Telegram custom emoji";
    image.decoding = "async";
    image.src = url;
    node.appendChild(image);
  }

  makeObjectUrl(data, mime) {
    const url = URL.createObjectURL(new Blob([data], { type: mime }));
    this.objectUrls.add(url);
    return url;
  }

  restoreFallback(node, fallback) {
    node.style.removeProperty("width");
    node.style.removeProperty("height");
    node.classList.add("tg-custom-emoji-fallback");
    node.textContent = fallback;
  }

  setSize(node, size) {
    const safeSize = Math.max(1, Math.min(512, Number(size) || 22));
    node.style.width = `${safeSize}px`;
    node.style.height = `${safeSize}px`;
  }
}

module.exports = TelegramCustomEmojiPlugin;
