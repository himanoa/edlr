// INARA 手動同期ボタン。api.action("resync") で plugin 本体の
// on-message(driver="dashboard", topic="resync") が届く。
export default function mount(el, api) {
  el.innerHTML = `
    <button type="button" class="rounded-lg border px-4 py-2 hover:bg-accent">
      現行セッションを再送
    </button>
    <p class="mt-2 text-sm text-muted-foreground" data-status>
      未送信分を INARA へ送り直します。進捗はログ画面で確認できます。
    </p>
  `;
  const status = el.querySelector("[data-status]");
  el.querySelector("button").addEventListener("click", () => {
    api.action("resync");
    status.textContent =
      "再送をリクエストしました(" +
      new Date().toLocaleTimeString() +
      ")。結果はログ画面で確認できます。";
  });
}
