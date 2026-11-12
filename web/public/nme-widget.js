/*! NME widget: <script src="https://<nme-site>/nme-widget.js" data-ticker="pepe"></script>
 * Drops a live coin card (war status, Buy, Hire) where the script tag is. The card runs in an iframe from the NME
 * site, so the host page never sees wallet data, and it stores no keys. Optional: data-width (default 420). */
(function () {
  var me = document.currentScript;
  if (!me) return;
  var origin = new URL(me.src).origin;
  var tags = document.querySelectorAll('script[src$="nme-widget.js"][data-ticker]');
  tags.forEach(function (s) {
    if (s.getAttribute("data-nme-mounted") || !s.isConnected) return;
    s.setAttribute("data-nme-mounted", "1");
    var ticker = String(s.getAttribute("data-ticker") || "").trim().toLowerCase();
    if (!/^[a-z0-9._-]{2,64}$/.test(ticker)) return;
    var f = document.createElement("iframe");
    f.src = origin + "/widget/" + encodeURIComponent(ticker);
    f.title = "$" + ticker.toUpperCase() + " on NME";
    f.loading = "lazy";
    f.allow = "clipboard-write";
    f.style.cssText = "display:block;width:100%;max-width:" + (parseInt(s.getAttribute("data-width"), 10) || 420) + "px;height:520px;border:0;background:transparent;color-scheme:light";
    s.parentNode.insertBefore(f, s.nextSibling);
    window.addEventListener("message", function (e) {
      if (e.origin !== origin || e.source !== f.contentWindow || !e.data || e.data.type !== "nme:height") return;
      f.style.height = Math.min(1400, Math.max(200, e.data.h)) + "px";
    });
  });
})();
