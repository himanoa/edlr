// 直近の FSDJump を表示する。イベントは manifest `events` フィルタ通過分だけ届く。
export default function mount(el, api) {
  el.innerHTML = `
    <div class="text-xl font-semibold" data-system>—</div>
    <div class="text-sm text-muted-foreground" data-time>FSDJump 待ち</div>
  `;
  const system = el.querySelector("[data-system]");
  const time = el.querySelector("[data-time]");
  api.onEvent((event) => {
    if (event.kind !== "journal" || event.event !== "FSDJump") return;
    system.textContent = (event.raw && event.raw.StarSystem) || "?";
    time.textContent = event.timestamp || "";
  });
}
