import { RpcClient } from "@/rpc";
import { DriversList } from "@/types/plugin";
import { defaultWsUrl } from "@/ws";
import { atom } from "jotai";

export const createDriverList$ = (
  makeClient: () => Pick<RpcClient, "call" | "close"> = () => new RpcClient(defaultWsUrl()),
) => {
  const fetch$ = atom(async () => {
    const client = makeClient();
    try {
      return await client.call<DriversList>("drivers/list");
    } finally {
      client.close();
    }
  });

  // ミューテーション後の差し替え用。null の間は fetch$ の結果がそのまま見える。
  const local$ = atom<DriversList | null>(null);

  return atom(
    (get) => get(local$) ?? get(fetch$),
    async (get, set, update: (prev: DriversList) => DriversList) => {
      const prev = get(local$) ?? (await get(fetch$));
      set(local$, update(prev));
    },
  );
};

export const driverList$ = createDriverList$();
