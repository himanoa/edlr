import { RpcClient } from "@/rpc";
import { defaultWsUrl } from "@/ws";
import { createRpcResource$ } from "./rpcResource";

export const createWidgets$ = (
  makeClient: () => Pick<RpcClient, "listDashboard" | "close"> = () =>
    new RpcClient(defaultWsUrl()),
) => createRpcResource$((client) => client.listDashboard(), makeClient);

export const widgets$ = createWidgets$()
