const test = require("node:test");
const assert = require("node:assert/strict");
const {
  insertTelegramBlock,
  removeLegacyEventMarkers,
} = require("./telegram-blocks");

test("removes legacy event marker lines", () => {
  const source = [
    "## Лента Telegram",
    "",
    "<!-- tg-event:-1001:17 -->",
    "- 23:41 · 🐶 [↗](https://t.me/c/1/17)",
    "<!-- /tg-event:-1001:17 -->",
    "",
  ].join("\n");
  const result = removeLegacyEventMarkers(source);
  assert.equal(result.includes("tg-event"), false);
  assert.equal(result.includes("https://t.me/c/1/17"), true);
});

test("deduplicates by Telegram message link without visible metadata", () => {
  const markdown = "- 23:41 · 🐶 [↗](https://t.me/c/1/17)";
  const source = `## Лента Telegram\n\n${markdown}\n`;
  const result = insertTelegramBlock(source, { markdown });
  assert.equal(result, source);
  assert.equal(result.match(/https:\/\/t\.me\/c\/1\/17/g).length, 1);
});

test("inserts a new event before the next heading", () => {
  const markdown = "- 00:01 · hello [↗](https://t.me/c/1/18)";
  const source = "# Day\n\n## Лента Telegram\n\n## Итог дня\n";
  const result = insertTelegramBlock(source, { markdown });
  assert.ok(result.indexOf(markdown) < result.indexOf("## Итог дня"));
  assert.equal(result.includes("tg-event"), false);
});

