import { RpcClient } from "@/rpc";
import { DriversList } from "@/types/plugin";
import { defaultWsUrl } from "@/ws";
import { createRpcResource$ } from "./rpcResource";

export const createDriverList$ = (
  makeClient: () => Pick<RpcClient, "call" | "close"> = () => new RpcClient(defaultWsUrl()),
) => createRpcResource$((client) => client.call<DriversList>("drivers/list"), makeClient);

export const driverList$ = createDriverList$();
