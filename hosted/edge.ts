import { isIP } from "node:net";

// Accept only a single IP supplied by the server's trusted ingress boundary.
// IPv4-mapped addresses and IPv6 addresses on the same /64 share a budget.
export function clientNetwork(address?: string): string {
  if (!address || address.length > 45) return "";
  const version = isIP(address);
  if (version === 4) return address;
  if (version !== 6 || address.includes("%")) return "";
  const canonical = new URL(`http://[${address}]`).hostname.slice(1, -1);
  const [head, tail] = canonical.split("::");
  const groups = head ? head.split(":") : [];
  if (tail !== undefined) {
    const suffix = tail ? tail.split(":") : [];
    while (groups.length + suffix.length < 8) groups.push("0");
    groups.push(...suffix);
  }
  if (groups.slice(0, 5).every((group) => group === "0") && groups[5] === "ffff") {
    const high = parseInt(groups[6], 16);
    const low = parseInt(groups[7], 16);
    return `${high >> 8}.${high & 255}.${low >> 8}.${low & 255}`;
  }
  return `${groups[0]}:${groups[1]}:${groups[2]}:${groups[3]}::/64`;
}

export async function requestJson(request: Request, max: number) {
  const length = request.headers.get("content-length");
  if (length !== null && !/^\d+$/.test(length)) return 400;
  if (length !== null && Number(length) > max) return 413;
  if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() !== "application/json" ||
      !request.body) return 400;
  const reader = request.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let bytes = 0;
  let text = "";
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    void reader.cancel().catch(() => {});
  }, 5000);
  try {
    for (;;) {
      const chunk = await reader.read();
      if (timedOut) return 408;
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > max) return 413;
      text += decoder.decode(chunk.value, { stream: true });
    }
    const data: unknown = JSON.parse(text + decoder.decode());
    return data !== null && typeof data === "object" && !Array.isArray(data) ? data as Record<string, unknown> : 400;
  } catch {
    return timedOut ? 408 : 400;
  } finally {
    clearTimeout(timer);
    void reader.cancel().catch(() => {});
  }
}

const testMessage = "Milo test: Rex alert mirroring can reach this destination. This is not a Rex alert, and nothing else was sent.";
const code = "<code>(?:[^<>&\\n]|&(?:amp|lt|gt);)+</code>";
const alert = new RegExp(`^Rex: (?:a program|a terminal|${code}) (?:is waiting for (?:permission|an answer|you(?: to sign in)?)|reported an error|finished|sent a notification|rang its bell)(?:\\n${code})?(?:\\n\\([0-9]{1,10} earlier (?:alert was|alerts were) not sent\\.\\))?$`);
const markup = /<code>|<\/code>|&(?:amp|lt|gt);|[<>&]/g;

// Program-derived text stays inside literal code spans, where Telegram won't
// make links, mentions, or commands. The production bot sends only Milo alerts.
export function validTelegramText(text: string, production: boolean): boolean {
  if (!text.trim() || text.length > 16_384 || /[\x00-\x09\x0b-\x1f\x7f-\x9f\p{Surrogate}\p{Bidi_Control}]/u.test(text)) return false;
  if (production && text !== testMessage && !alert.test(text)) return false;
  let inCode = false;
  let length = text.length;
  for (const { 0: token } of text.matchAll(markup)) {
    if (token === "<code>" || token === "</code>") {
      if (inCode === (token === "<code>")) return false;
      inCode = !inCode;
      length -= token.length;
    } else if (token.length === 1) return false;
    else length -= token.length - 1;
  }
  return !inCode && length > 0 && length <= 4096;
}
