import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 当前凭据后端（"windows" | "onepassword"）。
 * 纯展示用途（文案切换、置灰）；读取失败按 windows 处理（F4-4 的
 * fail-closed 只用于安全判定，这里只是界面说明，fail-open 无害）。
 */
export function useSecretBackend(): string {
  const [backend, setBackend] = useState("windows");
  useEffect(() => {
    invoke<string>("secret_backend_name")
      .then(setBackend)
      .catch(() => setBackend("windows"));
  }, []);
  return backend;
}
