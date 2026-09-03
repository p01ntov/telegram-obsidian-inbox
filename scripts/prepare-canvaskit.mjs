import { gzipSync } from "node:zlib";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const sourceUrl = new URL(
  "../node_modules/canvaskit-wasm/bin/full/canvaskit.wasm",
  import.meta.url,
);
const targetUrl = new URL("../src/canvaskit-full.wasm.gz", import.meta.url);
const wasm = await readFile(sourceUrl);
const compressed = gzipSync(wasm, { level: 9 });
await writeFile(targetUrl, compressed);

console.log(
  `Prepared CanvasKit WASM: ${wasm.length} -> ${compressed.length} bytes (${fileURLToPath(targetUrl)})`,
);
