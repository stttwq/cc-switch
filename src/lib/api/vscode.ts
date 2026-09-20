import { invoke } from "@tauri-apps/api/core";
import type { AppId } from "./types";

export const vscodeApi = {
  async getLiveProviderSettings(appId: AppId) {
    return await invoke("read_live_provider_settings", { app: appId });
  },
};
