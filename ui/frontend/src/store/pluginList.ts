import { RpcClient } from "@/rpc";
import { PluginsList } from "@/types/plugin";
import { defaultWsUrl } from "@/ws";
import { createRpcResource$ } from "./rpcResource";

export const createPluginList$ = (
  makeClient: () => Pick<RpcClient, "call" | "close"> = () => new RpcClient(defaultWsUrl()),
) => createRpcResource$((client) => client.call<PluginsList>("plugins/list"), makeClient);

export const pluginList$ = createPluginList$();
