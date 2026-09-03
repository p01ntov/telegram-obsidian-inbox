const CanvasKitModule = require("canvaskit-wasm/full");
const CanvasKitWasmGzipModule = require("./canvaskit-full.wasm.gz");
const { gunzipSync } = require("fflate");

const CanvasKitInit = CanvasKitModule.default || CanvasKitModule;
const compressedWasm = CanvasKitWasmGzipModule.default || CanvasKitWasmGzipModule;

let canvasKitPromise = null;

function loadCanvasKit() {
  if (canvasKitPromise) return canvasKitPromise;

  canvasKitPromise = (async () => {
    const wasmBytes = gunzipSync(compressedWasm);
    const wasmUrl = URL.createObjectURL(
      new Blob([wasmBytes], { type: "application/wasm" }),
    );
    try {
      return await CanvasKitInit({ locateFile: () => wasmUrl });
    } finally {
      URL.revokeObjectURL(wasmUrl);
    }
  })().catch((error) => {
    canvasKitPromise = null;
    throw error;
  });

  return canvasKitPromise;
}

class SkottieRuntime {
  constructor() {
    this.entries = new Set();
    this.rafId = null;
    this.destroyed = false;
    this.handleVisibilityChange = () => {
      if (!document.hidden) this.schedule();
    };
    document.addEventListener("visibilitychange", this.handleVisibilityChange);

    this.intersectionObserver =
      typeof IntersectionObserver === "function"
        ? new IntersectionObserver((changes) => {
            for (const change of changes) {
              const entry = change.target.__telegramSkottieEntry;
              if (entry) entry.visible = change.isIntersecting;
            }
            this.schedule();
          })
        : null;
  }

  async mount(node, animationData, size, label = "Telegram animation") {
    const container = document.createElement("span");
    container.className = "tg-custom-emoji-skottie";
    const canvas = document.createElement("canvas");
    canvas.className = "tg-custom-emoji-skottie-canvas";
    canvas.setAttribute("role", "img");
    canvas.setAttribute("aria-label", label);
    container.appendChild(canvas);
    node.appendChild(container);

    const canvasKit = await loadCanvasKit();
    if (this.destroyed || !node.isConnected || !canvas.isConnected) return;

    const dpr = Math.max(1, Math.min(2, window.devicePixelRatio || 1));
    const cssSize = Math.max(1, Math.min(512, Number(size) || 22));
    canvas.width = Math.max(1, Math.round(cssSize * dpr));
    canvas.height = Math.max(1, Math.round(cssSize * dpr));

    const makeSurface =
      typeof canvasKit.MakeSWCanvasSurface === "function"
        ? canvasKit.MakeSWCanvasSurface.bind(canvasKit)
        : canvasKit.MakeCanvasSurface.bind(canvasKit);
    const surface = makeSurface(canvas);
    if (!surface) throw new Error("Skottie could not create a canvas surface");

    const json =
      typeof animationData === "string"
        ? animationData
        : JSON.stringify(animationData);
    const animation = canvasKit.MakeManagedAnimation(json, {});
    if (!animation) {
      this.disposeSurface(surface);
      throw new Error("Skottie could not parse the Lottie animation");
    }

    const [animationWidth, animationHeight] = animation.size();
    const scale = Math.min(
      canvas.width / Math.max(1, animationWidth),
      canvas.height / Math.max(1, animationHeight),
    );
    const drawWidth = animationWidth * scale;
    const drawHeight = animationHeight * scale;
    const left = (canvas.width - drawWidth) / 2;
    const top = (canvas.height - drawHeight) / 2;
    const fps = Math.max(1, animation.fps() || 60);
    const durationSeconds = Math.max(1 / fps, animation.duration() || 1);
    const entry = {
      node,
      canvas,
      canvasKit,
      surface,
      animation,
      fps,
      durationSeconds,
      target: canvasKit.LTRBRect(left, top, left + drawWidth, top + drawHeight),
      startedAt: performance.now(),
      lastRenderedAt: Number.NEGATIVE_INFINITY,
      visible: true,
    };

    canvas.__telegramSkottieEntry = entry;
    this.entries.add(entry);
    this.intersectionObserver?.observe(canvas);
    this.draw(entry, entry.startedAt);
    this.schedule();
  }

  draw(entry, now) {
    const frameInterval = 1000 / Math.min(60, entry.fps);
    if (now - entry.lastRenderedAt + 0.25 < frameInterval) return;

    const elapsedSeconds = Math.max(0, now - entry.startedAt) / 1000;
    const animationSeconds = elapsedSeconds % entry.durationSeconds;
    const frame = animationSeconds * entry.fps;
    const canvas = entry.surface.getCanvas();
    canvas.clear(entry.canvasKit.TRANSPARENT);
    entry.animation.seekFrame(frame);
    entry.animation.render(canvas, entry.target);
    entry.surface.flush();
    entry.lastRenderedAt = now;
  }

  tick(now) {
    this.rafId = null;
    if (this.destroyed) return;

    let hasVisibleEntry = false;
    for (const entry of [...this.entries]) {
      if (!entry.node.isConnected || !entry.canvas.isConnected) {
        this.disposeEntry(entry);
        continue;
      }
      if (!entry.visible) continue;
      hasVisibleEntry = true;
      this.draw(entry, now);
    }

    if (hasVisibleEntry && !document.hidden) this.schedule();
  }

  schedule() {
    if (this.destroyed || document.hidden || this.rafId !== null) return;
    if (this.entries.size === 0) return;
    this.rafId = window.requestAnimationFrame((now) => this.tick(now));
  }

  disposeSurface(surface) {
    if (typeof surface.dispose === "function") surface.dispose();
    else surface.delete();
  }

  disposeEntry(entry) {
    if (!this.entries.delete(entry)) return;
    this.intersectionObserver?.unobserve(entry.canvas);
    delete entry.canvas.__telegramSkottieEntry;
    try {
      entry.animation.delete();
    } catch (_) {}
    try {
      this.disposeSurface(entry.surface);
    } catch (_) {}
  }

  destroy() {
    this.destroyed = true;
    document.removeEventListener("visibilitychange", this.handleVisibilityChange);
    this.intersectionObserver?.disconnect();
    if (this.rafId !== null) window.cancelAnimationFrame(this.rafId);
    this.rafId = null;
    for (const entry of [...this.entries]) this.disposeEntry(entry);
  }
}

module.exports = { SkottieRuntime, loadCanvasKit };
