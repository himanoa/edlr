import { atom } from "jotai";

/**
 * 「初回読み取りは RPC で取得(Suspense 対応)、以後はミューテーションの応答で
 * 差し替え」という resource atom の共通形。
 *
 * - read: local が空なら fetch の Promise を返す(コンポーネント側はサスペンド)
 * - write: `(prev) => next` を受けて local に確定値を積む。以後の read は同期値
 * - write に値を直接渡すと fetch を経由せず local に積む(テストの seed 用)
 * - fetch 用クライアントは取得のたびに作って使い終わったら close する
 */
export const createRpcResource$ = <T, C extends { close(): void }>(
  fetcher: (client: C) => Promise<T>,
  makeClient: () => C,
) => {
  const fetch$ = atom(async () => {
    const client = makeClient();
    try {
      return await fetcher(client);
    } finally {
      client.close();
    }
  });

  const local$ = atom<T | null>(null);

  return atom(
    (get) => get(local$) ?? get(fetch$),
    async (get, set, update: T | ((prev: T) => T)) => {
      if (typeof update !== "function") {
        set(local$, update);
        return;
      }
      const prev = get(local$) ?? (await get(fetch$));
      set(local$, (update as (prev: T) => T)(prev));
    },
  );
};
