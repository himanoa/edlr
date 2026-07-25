import PluginForm from "../components/PluginForm";
import { mockPlugins } from "../mock/plugins";

export default function Plugins() {
  return (
    <section>
      <h1>Plugins</h1>
      <p className="note">
        ※ 現在はモックデータです。プラグイン基盤の実装後に本物のマニフェストと接続します。
      </p>
      {mockPlugins.map((p) => (
        <article key={p.id} className="plugin-card">
          <h2>{p.name}</h2>
          <p>{p.description}</p>
          <PluginForm manifest={p} />
        </article>
      ))}
    </section>
  );
}
