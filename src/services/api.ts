declare const __IS_TAURI__: boolean;

import type * as TauriApi from "./tauriApi";
import { isRemoteNodeActive } from "./nodeConfig";

type ApiModule = typeof TauriApi;

const LOCAL_ONLY_METHODS = new Set<PropertyKey>(["getInstallType", "writeExportFile"]);

function getApiModule(prop: PropertyKey): Promise<ApiModule> {
  if (__IS_TAURI__ && (LOCAL_ONLY_METHODS.has(prop) || !isRemoteNodeActive())) {
    return import("./tauriApi");
  }
  return import("./webApi") as Promise<ApiModule>;
}

export const api = new Proxy({} as ApiModule, {
  get(_target, prop) {
    return async (...args: unknown[]) => {
      const apiModule = await getApiModule(prop);
      const member = apiModule[prop as keyof ApiModule];

      if (typeof member !== "function") {
        return member;
      }

      return (member as (...innerArgs: unknown[]) => unknown)(...args);
    };
  },
});
