// One rebuilt page, by the hash of what is in it.
//
// Named by its content, so the answer can be cached for as long as anybody
// likes: a different page is a different name. Asked for only by a reader
// whose page is out of date, which is a small fraction of readers.
import { getStore } from "@netlify/blobs";
import type { Context } from "@netlify/functions";

export default async (request: Request, _context: Context) => {
  const hash = new URL(request.url).pathname.split("/").pop() ?? "";
  if (!/^[a-z0-9]{4,64}$/.test(hash)) {
    return new Response("not a page name\n", { status: 400 });
  }
  const store = getStore("talimist");
  const html = await store.get(`page/${hash}`, { type: "text" });
  if (html === null) {
    return new Response("no such page\n", { status: 404 });
  }
  return new Response(html, {
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "public, max-age=31536000, immutable",
    },
  });
};

export const config = { path: "/netlify/page/:hash" };
