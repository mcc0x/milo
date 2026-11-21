import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface BindingStore {
  load(): { digest: string; chat: number }[];
  insert(digest: string, chat: number): void;
  remove(digest: string): void;
  revoke(chat: number): void;
  close(): void;
}

// A bot token rotation preserves bindings; a different bot must never inherit them.
export function openBindings(botId: string, directory = join(homedir(), ".local", "state", "milo", "relay")): BindingStore {
  if (!/^\d+$/.test(botId)) throw new Error("Invalid bot ID");
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const db = new Database(join(directory, `telegram-${botId}.sqlite`), { create: true });
  db.exec("PRAGMA secure_delete = ON; CREATE TABLE IF NOT EXISTS bindings (digest TEXT PRIMARY KEY, chat INTEGER NOT NULL)");
  const all = db.query<{ digest: string; chat: number }, []>("SELECT digest, chat FROM bindings");
  const insert = db.query("INSERT INTO bindings(digest,chat) VALUES (?,?)");
  const remove = db.query("DELETE FROM bindings WHERE digest=?");
  const revoke = db.query("DELETE FROM bindings WHERE chat=?");
  return { load: () => all.all(), insert: (digest: string, chat: number) => insert.run(digest, chat),
    remove: (digest: string) => remove.run(digest), revoke: (chat: number) => revoke.run(chat), close: () => db.close() };
}
