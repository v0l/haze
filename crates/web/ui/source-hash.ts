import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

export const SOURCE_FILES = ["index.html", "package.json", "bun.lock", "tsconfig.json", "vite.config.ts", "source-hash.ts"];
export const SOURCE_DIRS = ["src"];

function walk(root: string, dir: string, out: string[]): void {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(root, path, out);
    else out.push(relative(root, path).split(sep).join("/"));
  }
}

export function sourceFiles(root: string): string[] {
  const files = SOURCE_FILES.filter((f) => {
    try {
      return statSync(join(root, f)).isFile();
    } catch {
      return false;
    }
  });
  for (const dir of SOURCE_DIRS) walk(root, join(root, dir), files);
  return files.sort();
}

export function sourceHash(root: string): string {
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  let hash = 0xcbf29ce484222325n;
  const feed = (bytes: Uint8Array) => {
    for (const byte of bytes) hash = ((hash ^ BigInt(byte)) * prime) & mask;
  };
  const encoder = new TextEncoder();
  for (const file of sourceFiles(root)) {
    feed(encoder.encode(file));
    feed(new Uint8Array([0]));
    feed(new Uint8Array(readFileSync(join(root, file))));
    feed(new Uint8Array([0]));
  }
  return hash.toString(16).padStart(16, "0");
}
