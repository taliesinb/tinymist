// Takes a page that is out of date and makes it the page it should be.
//
// A site on Netlify is deployed whole, so a page rebuilt since the deploy is
// kept in a blob and listed in the patchset instead. This asks for that list
// when the browser has nothing else to do, and if this page is in it, fetches
// the new one and swaps it in. A reader who never annotates and never lands on
// a rebuilt page pays one cached request for this and nothing else.
(() => {
  const MANIFEST = "/netlify/patchset.json";
  const PAGE = "/netlify/page/";
  // What this page is, as the site names it, and what version of it this is.
  const said = (name) => {
    const meta = document.querySelector(`meta[name="${name}"]`);
    return (meta && meta.content) || "";
  };
  const here = () => location.pathname;

  // Where the page's own content lives, so that what is swapped is the
  // document rather than the furniture around it: the annotator's overlay, a
  // navigation bar, anything else the site puts on the page.
  const bodyOf = (doc) =>
    doc.querySelector("#tinymist-doc") ||
    doc.querySelector("main") ||
    doc.body;

  const swap = (html) => {
    const fresh = new DOMParser().parseFromString(html, "text/html");
    const from = bodyOf(fresh);
    const into = bodyOf(document);
    if (!from || !into) return false;
    into.replaceChildren(...Array.from(from.childNodes));
    if (fresh.title) document.title = fresh.title;
    // The page now says what it now is, so a second look at the patchset does
    // not fetch the same page again.
    const stamp = fresh.querySelector('meta[name="tm-build"]');
    const mine = document.querySelector('meta[name="tm-build"]');
    if (stamp && mine) mine.content = stamp.content;
    return true;
  };

  const patch = async () => {
    let patchset;
    try {
      const said = await fetch(MANIFEST, { headers: { accept: "application/json" } });
      if (!said.ok) return;
      patchset = await said.json();
    } catch (err) {
      // Offline, or the site has no patchset: the page stays as it was built,
      // which is a page.
      return;
    }
    const wanted = (patchset.pages || {})[here()];
    if (!wanted || !wanted.hash) return;
    if (wanted.hash === said("tm-page")) return;
    try {
      const got = await fetch(PAGE + encodeURIComponent(wanted.hash));
      if (!got.ok) return;
      if (!swap(await got.text())) return;
      // Whatever is drawn over the document has to be drawn again: the
      // annotator listens for this and re-reads the page.
      document.dispatchEvent(
        new CustomEvent("tm-patched", { detail: { hash: wanted.hash, time: wanted.time } }),
      );
    } catch (err) {
      /* a page that cannot be fetched is a page that stays as it was */
    }
  };

  // When the browser has nothing better to do. A page that is being read is
  // more important than a page that is up to date, and the difference is
  // usually a heading.
  const soon = window.requestIdleCallback || ((fn) => setTimeout(fn, 1200));
  soon(() => patch(), { timeout: 5000 });
})();
