// What has changed since the deploy, for a page that wants to know.
//
// One object for the whole site, so every reader gets the same answer and the
// answer can sit in the edge cache. That is the point of this being an edge
// function rather than a lookup on every request: a page asks for a static
// file and gets it without any code running, and this one small thing runs
// about once a minute per location however many pages are read.
import { getStore } from "@netlify/blobs";

const EMPTY = '{"build":"","pages":{}}';

export default async () => {
  let patchset = EMPTY;
  try {
    const store = getStore("talimist");
    patchset = (await store.get("patchset", { type: "text" })) ?? EMPTY;
  } catch (err) {
    // A store that cannot be read is a site with nothing to patch, which is
    // what an unpatched site looks like anyway.
    console.warn("talimist: cannot read the patchset", err);
  }
  return new Response(patchset, {
    headers: {
      "content-type": "application/json",
      // Held at the edge for a minute, and served stale while it is fetched
      // again for up to ten: a page that is a minute behind is a page that
      // will be told a minute from now.
      "netlify-cdn-cache-control": "public, s-maxage=60, stale-while-revalidate=600",
      // The browser revalidates, so a reader who stays on a page sees the
      // patch when it lands rather than when their cache expires.
      "cache-control": "public, max-age=0, must-revalidate",
    },
  });
};

export const config = { path: "/_talimist/patchset.json" };
