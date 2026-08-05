import { RpcClient } from "@/rpc";
import { defaultWsUrl } from "@/ws";
import { atom } from "jotai";

export const createRpcClient$ = (
  makeClient: () => RpcClient = () => new RpcClient(defaultWsUrl()),
) => {
  const client$ = atom<RpcClient | null>(null);

  // 最初の購読者が現れたら接続し、最後の購読者が消えたら切断する。
  client$.onMount = (setAtom) => {
    const client = makeClient();
    setAtom(client);
    return () => {
      setAtom(null);
      client.close();
    };
  };

  return client$;
};

export const rpcClient$ = createRpcClient$();
