import { invoke } from "@tauri-apps/api/core";
import type { AppId } from "./types";

export const vscodeApi = {
  async getLiveProviderSettings(appId: AppId) {
    return await invoke("read_live_provider_settings", { app: appId });
  },

  async exportConfigToFile(filePath: string) {
    return await invoke("export_config_to_file", {
      filePath,
    });
  },

  async importConfigFromFile(filePath: string) {
    return await invoke("import_config_from_file", {
      filePath,
    });
  },

  async saveFileDialog(defaultName: string): Promise<string | null> {
    return await invoke("save_file_dialog", {
      defaultName,
    });
  },

  async openFileDialog(): Promise<string | null> {
    return await invoke("open_file_dialog");
  },
};
