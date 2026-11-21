import { Database } from "bun:sqlite";
import { randomBytes } from "node:crypto";
import {
  accessSync, closeSync, constants, fchmodSync, fstatSync, lstatSync, mkdirSync,
  openSync, readFileSync, realpathSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface BindingStore {
  readonly issuer: string;
  load(limit: number): { digest: string; chat: number }[];
  insert(digest: string, chat: number): void;
  remove(digest: string): void;
  removeIssued(digest: string, issuer: string): boolean;
  revoke(chat: number): void;
  close(): void;
}

// Railway mounts the volume only at runtime. An image directory (even a
// writable one) must never stand in for it, nor may a tmpfs be used for bindings.
export function verifyDataMount(directory: string) {
  if (process.platform !== "linux" || process.getuid!() === 0 || realpathSync(directory) !== directory) {
    throw new Error("Production data must be on a Linux volume mount.");
  }
  const mounts = readFileSync("/proc/self/mountinfo", "utf8").split("\n")
    .map((line) => line.split(" "))
    .filter((fields) => fields[4] === directory);
  if (!mounts.length || mounts.some((fields) => {
    const separator = fields.indexOf("-");
    return !fields[5].split(",").includes("rw") || separator < 0 ||
      ["tmpfs", "ramfs", "overlay"].includes(fields[separator + 1]);
  })) throw new Error("Production data must be on a writable persistent volume mount.");
  const state = lstatSync(directory);
  if (!state.isDirectory() || state.uid !== process.getuid!() || (state.mode & 0o7777) !== 0o700) {
    throw new Error("Production volume must be owner-only (0700).");
  }
  accessSync(directory, constants.R_OK | constants.W_OK | constants.X_OK);
}

function secureFile(path: string, create = false) {
  let fd: number;
  try {
    fd = openSync(path, constants.O_RDWR | constants.O_NOFOLLOW | constants.O_NONBLOCK |
      (create ? constants.O_CREAT : 0), 0o600);
  } catch (error) {
    if (!create && (error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  try {
    const file = fstatSync(fd, { bigint: true });
    if (!file.isFile() || file.uid !== BigInt(process.getuid!()) || file.nlink !== 1n) {
      throw new Error("Invalid binding database file.");
    }
    if ((file.mode & 0o7777n) !== 0o600n) fchmodSync(fd, 0o600);
    return file;
  } finally {
    closeSync(fd);
  }
}
// A bot token rotation preserves bindings; a different bot must never inherit them.
export function openBindings(botId: string, directory = join(homedir(), ".local", "state", "milo", "relay")): BindingStore {
  if (!/^\d+$/.test(botId)) throw new Error("Invalid bot ID");
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const fd = openSync(directory, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_DIRECTORY);
  try {
    const state = fstatSync(fd);
    if (state.uid !== process.getuid!()) throw new Error("Invalid binding database directory.");
    if ((state.mode & 0o7777) !== 0o700) fchmodSync(fd, 0o700);
  } finally {
    closeSync(fd);
  }
  const path = join(directory, `telegram-${botId}.sqlite`);
  const original = secureFile(path, true)!;
  function hasOriginalFile() {
    try {
      const current = lstatSync(path, { bigint: true });
      return current.isFile() && current.dev === original.dev && current.ino === original.ino && current.nlink === 1n;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      return false;
    }
  }
  for (const suffix of ["-journal", "-wal", "-shm"]) secureFile(path + suffix);
  const db = new Database(path, { create: false });
  try {
    db.exec(`PRAGMA journal_mode = DELETE; PRAGMA secure_delete = ON;
      CREATE TABLE IF NOT EXISTS bindings (digest TEXT PRIMARY KEY, chat INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS binding_ledger (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        issuer TEXT NOT NULL
      )`);
    const readIssuer = db.query<{ issuer: string }, []>("SELECT issuer FROM binding_ledger WHERE singleton=1");
    const initializeIssuer = db.transaction(() => {
      const row = readIssuer.get();
      if (row) return row.issuer;
      const issuer = randomBytes(12).toString("base64url");
      db.run("INSERT INTO binding_ledger (singleton, issuer) VALUES (1, ?)", issuer);
      return issuer;
    });
    const issuer = initializeIssuer.immediate();
    if (!/^[A-Za-z0-9_-]{16}$/.test(issuer)) throw new Error("Invalid binding ledger.");
    const all = db.query<{ digest: string; chat: number }, [number]>("SELECT digest, chat FROM bindings LIMIT ?");
    const insert = db.query("INSERT INTO bindings(digest,chat) VALUES (?,?)");
    const remove = db.query("DELETE FROM bindings WHERE digest=?");
    const revoke = db.query("DELETE FROM bindings WHERE chat=?");
    type Mutation = "insert" | "remove" | "revoke";
    const mutate = db.transaction((expectedIssuer: string, action: Mutation, key: string | number, chat?: number) => {
      if (readIssuer.get()?.issuer !== expectedIssuer) return false;
      switch (action) {
        case "insert": insert.run(key, chat!); break;
        case "remove": remove.run(key); break;
        case "revoke": revoke.run(key); break;
      }
      return true;
    });
    function write(expectedIssuer: string, action: Mutation, key: string | number, chat?: number) {
      return hasOriginalFile() && mutate.immediate(expectedIssuer, action, key, chat) && hasOriginalFile();
    }
    function requireWrite(ok: boolean) {
      if (!ok) throw new Error("Binding ledger no longer matches the configured database.");
    }
    return {
      issuer, load: (limit) => all.all(limit),
      insert: (digest, chat) => requireWrite(write(issuer, "insert", digest, chat)),
      remove: (digest) => requireWrite(write(issuer, "remove", digest)),
      removeIssued: (digest, expectedIssuer) => write(expectedIssuer, "remove", digest),
      revoke: (chat) => requireWrite(write(issuer, "revoke", chat)),
      close: () => db.close(),
    };
  } catch (error) {
    db.close();
    throw error;
  }
}
