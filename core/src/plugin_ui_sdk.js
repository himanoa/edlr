// edlr dashboard widget SDK.
//
// 使い方: <script src="/plugin-ui-sdk.js"></script> のあと
//   edlr.onEvent(function (event) { ... });  // {kind, timestamp?, event?, raw}
//   edlr.ready();  // 準備完了を親へ通知(これを呼ぶまでイベントは届かない)
//
// ウィジェットは sandbox="allow-scripts" の iframe(opaque origin)で動き、
// 親ダッシュボードとの通信は postMessage のみ。親はプラグインの manifest
// `events` にマッチするイベントだけを edlr:event で転送してくる。
(function () {
  "use strict";
  var listeners = [];
  var context = null; // {plugin, widget} — edlr:init で確定

  window.addEventListener("message", function (e) {
    var msg = e.data;
    if (!msg || typeof msg !== "object") return;
    if (msg.type === "edlr:init") {
      context = { plugin: msg.plugin, widget: msg.widget };
      return;
    }
    if (msg.type === "edlr:event") {
      for (var i = 0; i < listeners.length; i++) {
        try {
          listeners[i](msg.event);
        } catch (err) {
          // widget 側リスナーの例外で他のリスナーを止めない
        }
      }
    }
  });

  window.edlr = {
    ready: function () {
      window.parent.postMessage({ type: "edlr:ready" }, "*");
    },
    onEvent: function (cb) {
      listeners.push(cb);
    },
    // 任意: コンテンツの実高さを親へ伝え、iframe の高さを合わせてもらう
    reportHeight: function () {
      var h = document.documentElement.scrollHeight;
      window.parent.postMessage({ type: "edlr:height", px: h }, "*");
    },
    context: function () {
      return context;
    },
    // プラグインへのアクション要求。親が plugins/dashboard-action RPC に
    // 変換し、プラグインの on-message(driver="dashboard", topic=name) へ届く。
    // どのプラグインへ届くかは親(iframe を作った側)が決める — この iframe が
    // 属するプラグイン自身にしか届かない。
    action: function (name) {
      window.parent.postMessage({ type: "edlr:action", name: String(name) }, "*");
    },
  };
})();
