function removeLegacyEventMarkers(content) {
  return String(content || "").replace(
    /^[\t ]*<!--\s*\/?tg-event:[^>]*-->\s*(?:\r?\n|$)/gm,
    "",
  );
}

function telegramSourceLink(markdown) {
  const match = String(markdown || "").match(
    /\[↗\]\((https:\/\/t\.me\/[^)\s]+)\)/,
  );
  return match ? match[1] : "";
}

function insertTelegramBlock(content, event) {
  const cleaned = removeLegacyEventMarkers(content);
  const markdown = String(event?.markdown || "").trim();
  const sourceLink = telegramSourceLink(markdown);

  // Telegram message links are stable across devices, so they provide
  // cross-device deduplication without adding metadata to the note.
  if (
    (sourceLink && cleaned.includes(`(${sourceLink})`)) ||
    (markdown && cleaned.includes(markdown))
  ) {
    return cleaned === content ? content : `${cleaned.trimEnd()}\n`;
  }

  const heading = "## Лента Telegram";
  let result = cleaned;
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
  return `${before}\n\n${markdown}\n\n${after}`.trimEnd() + "\n";
}

module.exports = {
  insertTelegramBlock,
  removeLegacyEventMarkers,
  telegramSourceLink,
};

