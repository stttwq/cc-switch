import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Copy, Loader2, ShieldAlert } from "lucide-react";
import { settingsApi } from "@/lib/api/settings";
import { useSettingsQuery } from "@/lib/query";
import type { AppId } from "@/lib/api/types";

/** P2 严格模式三态。 */
type StrictMode = "off" | "perApp" | "global";

const STRICT_APPS: AppId[] = ["claude", "codex", "pi"];
const SHells = ["powershell", "cmd", "bash"] as const;
type Shell = (typeof SHells)[number];

/**
 * 环境变量投递的严格模式（B5 / P2 分级）。挂在"设置 → 高级"独立的安全小节里。
 *
 * 三态：关 / 按应用 / 全局。任一形态严格时，切换供应商都不把该应用密钥写进
 * `HKCU\Environment`；密钥改由「打开终端」或 `ccs env` shim 注入用户自己的 shell。
 * P1 的"复制激活命令"给出把 shim 接入 $PROFILE/.bashrc/cmd 的一次性绝对路径片段。
 */
export function EnvDeliverySection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: settings } = useSettingsQuery();
  const [busy, setBusy] = useState(false);
  const [shell, setShell] = useState<Shell>("powershell");

  const isGlobal = settings?.envDeliveryStrictMode ?? false;
  const perAppList = settings?.envDeliveryStrictApps ?? [];
  const mode: StrictMode = isGlobal
    ? "global"
    : perAppList.length > 0
      ? "perApp"
      : "off";

  const isAppStrict = (app: AppId) => isGlobal || perAppList.includes(app);
  const anyStrict = mode !== "off";

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["settings"] });

  const run = async (fn: () => Promise<unknown>, okMsg?: string) => {
    setBusy(true);
    try {
      await fn();
      if (okMsg) toast.success(okMsg);
      await invalidate();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const selectGlobal = () =>
    run(
      () => settingsApi.setEnvDeliveryStrictMode(true),
      t("settings.envDelivery.enabledToast"),
    );

  const selectOff = () =>
    run(
      () => settingsApi.setEnvDeliveryStrictMode(false),
      t("settings.envDelivery.disabledToast"),
    );

  const toggleApp = (app: AppId) => {
    const next = new Set(perAppList);
    if (next.has(app)) next.delete(app);
    else next.add(app);
    const list = STRICT_APPS.filter((a) => next.has(a));
    // 全清空则回到"关"，避免保存空"按应用"列表。
    void run(() =>
      list.length === 0
        ? settingsApi.setEnvDeliveryStrictMode(false)
        : settingsApi.setEnvDeliveryStrictApps(list),
    );
  };

  const copySnippet = async () => {
    try {
      const snippet = await settingsApi.getShimActivationSnippet(shell);
      await navigator.clipboard.writeText(snippet);
      toast.success(t("settings.envDelivery.copiedToast"));
    } catch (error) {
      toast.error(String(error));
    }
  };

  return (
    <div className="space-y-3">
      <div>
        <h3 className="text-base font-semibold">
          {t("settings.envDelivery.title")}
        </h3>
        <p className="text-sm text-muted-foreground">
          {t("settings.envDelivery.description")}
        </p>
      </div>

      {/* 三态选择 */}
      <div className="flex items-center justify-between gap-4">
        <span className="text-sm">{t("settings.envDelivery.modeLabel")}</span>
        {busy && (
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {(["off", "perApp", "global"] as StrictMode[]).map((m) => (
          <button
            key={m}
            type="button"
            disabled={busy}
            onClick={() =>
              m === "global"
                ? selectGlobal()
                : m === "off"
                  ? selectOff()
                  : perAppList.length === 0 && toggleApp("claude")
            }
            className={`rounded-md border px-3 py-1 text-sm ${
              mode === m
                ? "border-primary bg-primary/10"
                : "border-border hover:bg-muted"
            }`}
          >
            {t(`settings.envDelivery.mode.${m}`)}
          </button>
        ))}
      </div>

      {/* 按应用多选 */}
      {mode === "perApp" && (
        <div className="flex flex-wrap items-center gap-4 pl-1">
          {STRICT_APPS.map((app) => (
            <label key={app} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={isAppStrict(app)}
                disabled={busy}
                onChange={() => toggleApp(app)}
              />
              {t(`settings.envDelivery.app.${app}`)}
            </label>
          ))}
        </div>
      )}

      {anyStrict && (
        <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 text-xs text-amber-700 dark:text-amber-400">
          <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{t("settings.envDelivery.strictModeActive")}</span>
        </div>
      )}

      {/* P1：复制激活命令，把 ccs env shim 接入用户自己的 shell */}
      <div className="space-y-2 rounded-lg border border-border p-3">
        <div className="text-left">
          <span className="text-sm">
            {t("settings.envDelivery.copyCommand")}
          </span>
          <p className="text-xs text-muted-foreground">
            {t("settings.envDelivery.copyCommandHint")}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {SHells.map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => setShell(s)}
              className={`rounded px-2 py-1 text-xs ${
                shell === s
                  ? "bg-primary/10 text-primary"
                  : "text-muted-foreground"
              }`}
            >
              {s}
            </button>
          ))}
          <button
            type="button"
            onClick={copySnippet}
            className="ml-auto flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs hover:bg-muted"
          >
            <Copy className="h-3.5 w-3.5" />
            {t("settings.envDelivery.copyCommand")}
          </button>
        </div>
      </div>
    </div>
  );
}
