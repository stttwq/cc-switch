import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { TerminalSquare } from "lucide-react";
import { ToggleRow } from "@/components/ui/toggle-row";
import { settingsApi } from "@/lib/api/settings";

/**
 * 资源管理器右键「在此打开终端」层叠菜单开关。
 *
 * 开启 → 写 `HKCU\Software\Classes` 注册两处落点（文件夹 / 文件夹空白处），
 * 层叠子菜单含 Claude / Codex / Pi，点击后用各自当前激活的供应商在该目录起终端。
 * 关闭 → 删除注册表项。菜单标签随应用语言写入，凭据注入与 GUI「打开终端」同边界。
 */
export function ShellMenuSection() {
  const { t } = useTranslation();
  const [registered, setRegistered] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    settingsApi
      .isShellMenuRegistered()
      .then(setRegistered)
      .catch(() => setRegistered(false));
  }, []);

  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      if (next) {
        await settingsApi.registerShellMenu({
          root: t("settings.shellMenu.menuRoot"),
          claude: t("settings.shellMenu.menuClaude"),
          codex: t("settings.shellMenu.menuCodex"),
          pi: t("settings.shellMenu.menuPi"),
        });
        toast.success(t("settings.shellMenu.enabledToast"));
      } else {
        await settingsApi.unregisterShellMenu();
        toast.success(t("settings.shellMenu.disabledToast"));
      }
      setRegistered(next);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <ToggleRow
      icon={<TerminalSquare className="h-4 w-4 text-emerald-500" />}
      title={t("settings.shellMenu.title")}
      description={t("settings.shellMenu.description")}
      checked={registered}
      onCheckedChange={toggle}
      disabled={busy}
    />
  );
}
