// The annotations of one page.
//
// A blob per page: a page is what a reader has open and what a writer is
// writing about, so it is the unit that is read and the unit that is written.
// Writing is read-modify-write, and blobs have no locks, so a write says which
// version it was made against and is refused if that is no longer the current
// one — the caller reads again and tries again, which is what `mtime` in the
// answer is for.
import { getStore } from "@netlify/blobs";
import type { Context } from "@netlify/functions";

const EMPTY = { version: 1, annotations: [] };

/// Where a page's annotations are kept. The same rule as the Rust side: a path
/// too long to be a key is named by a hash of itself instead.
const keyFor = (path: string) => {
  const named = path.replace(/^\/+/, "").replace(/[^a-zA-Z0-9\-_./]/g, "_");
  if (named.length <= 500) return `annos/${named}`;
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(path)) {
    hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  }
  return `annos/long/${hash.toString(16).padStart(16, "0")}`;
};

export default async (request: Request, _context: Context) => {
  const url = new URL(request.url);
  const page = url.searchParams.get("page");
  if (!page) {
    return new Response("which page?\n", { status: 400 });
  }
  const store = getStore("talimist");
  const key = keyFor(page);

  if (request.method === "GET") {
    // Eventual consistency is right here: a reader who is a minute behind on
    // somebody else's annotation is a reader who will have it in a minute.
    const held = await store.getWithMetadata(key, { type: "json" });
    return Response.json(held?.data ?? EMPTY, {
      headers: {
        etag: held?.etag ?? "",
        "cache-control": "no-store",
      },
    });
  }

  if (request.method === "PUT") {
    const sent = await request.json();
    // What it was written against, so that two writers cannot lose each
    // other's work: the write is refused if the page has moved on, and the
    // caller reads again and writes again.
    const against = request.headers.get("if-match");
    const wrote = await store.set(key, JSON.stringify(sent), {
      ...(against ? { onlyIfMatch: against } : { onlyIfNew: true }),
    });
    if (!wrote.modified) {
      return new Response("somebody else wrote first; read it again\n", { status: 412 });
    }
    return new Response(null, { status: 204, headers: { etag: wrote.etag ?? "" } });
  }

  return new Response("GET to read, PUT to write\n", { status: 405 });
};

export const config = { path: "/netlify/annotations" };
