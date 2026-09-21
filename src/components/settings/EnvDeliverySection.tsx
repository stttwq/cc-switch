import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Loader2, ShieldAlert } from "lucide-react";
import { Switch } from "@/components/ui/switch";
import { settingsApi } from "@/lib/api/settings";
import { useSettingsQuery } from "@/lib/query";

/**
 * 环境变量投递的严格模式（B5 / 方案 2.4.7）。挂在"设置 → 高级"独立的安全小节里。
 *
 * 默认关。开启后切换供应商不再把密钥写进 `HKCU\Environment`，本机同用户的其他进程就读不到；
 * 密钥只经 cc-switch「打开终端」注入其自起的终端。代价：从别处启动的 CLI 拿不到密钥
 * （Codex 缺 `env_key`、Pi 变量未解析，都 fail-closed），所以必须从 cc-switch 开终端。
 */
export function EnvDeliverySection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: settings } = useSettingsQuery();
  const [busy, setBusy] = useState(false);

  const enabled = settings?.envDeliveryStrictMode ?? false;

  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      await settingsApi.setEnvDeliveryStrictMode(next);
      toast.success(
        next
          ? t("settings.envDelivery.enabledToast")
          : t("settings.envDelivery.disabledToast"),
      );
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
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

      <div className="flex items-center justify-between gap-4">
        <div className="text-left">
          <span className="text-sm">
            {t("settings.envDelivery.strictMode")}
          </span>
          <p className="text-xs text-muted-foreground">
            {t("settings.envDelivery.strictModeHint")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {busy && (
            <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          )}
          <Switch
            checked={enabled}
            onCheckedChange={(checked: boolean) => toggle(checked)}
            disabled={busy}
            aria-label={t("settings.envDelivery.strictMode")}
          />
        </div>
      </div>

      {enabled && (
        <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 text-xs text-amber-700 dark:text-amber-400">
          <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{t("settings.envDelivery.strictModeActive")}</span>
        </div>
      )}
    </div>
  );
}
