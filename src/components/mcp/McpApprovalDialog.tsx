import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff, ShieldAlert } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import type { McpServer } from "@/types";
import type { AppId } from "@/lib/api/types";

/**
 * SEC-A：MCP 导入内容审批确认框。
 *
 * 展示完整可滚动的执行语义字段（transport/type、command、逐项 args、cwd、
 * URL）；env 与 headers 的值默认掩码（可能含凭据），提供统一的显示开关；
 * 普通执行字段绝不因脱敏而隐藏（施工方案 §4.3-9）。确认由父组件以
 * expectedRevision 绑定预览修订提交，内容在确认前变化会被后端拒绝。
 */
interface McpApprovalDialogProps {
  isOpen: boolean;
  server: McpServer | undefined;
  app: AppId;
  pending?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

function isSensitiveKey(key: string): boolean {
  const lowered = key.toLowerCase();
  return ["key", "token", "secret", "password", "authorization"].some(
    (needle) => lowered.includes(needle),
  );
}

const McpApprovalDialog: React.FC<McpApprovalDialogProps> = ({
  isOpen,
  server,
  app,
  pending = false,
  onConfirm,
  onCancel,
}) => {
  const { t } = useTranslation();
  const [revealSecrets, setRevealSecrets] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setRevealSecrets(false);
    }
  }, [isOpen]);

  const spec = server?.server ?? {};
  const rows = useMemo(() => {
    const out: Array<{ label: string; value: string }> = [];
    if (spec.type) out.push({ label: "type", value: String(spec.type) });
    if (typeof spec.command === "string" && spec.command !== "")
      out.push({ label: "command", value: spec.command });
    if (Array.isArray(spec.args))
      spec.args.forEach((arg, index) =>
        out.push({ label: `args[${index}]`, value: String(arg) }),
      );
    if (typeof spec.cwd === "string" && spec.cwd !== "")
      out.push({ label: "cwd", value: spec.cwd });
    if (typeof spec.url === "string" && spec.url !== "")
      out.push({ label: "url", value: spec.url });
    return out;
  }, [spec]);

  const secretMaps = useMemo(() => {
    return [
      { label: "env", entries: Object.entries(spec.env ?? {}) },
      { label: "headers", entries: Object.entries(spec.headers ?? {}) },
    ].filter(({ entries }) => entries.length > 0);
  }, [spec]);

  if (!server) return null;

  const renderValue = (value: string) => {
    return (
      <span className="break-all font-mono text-xs">
        {revealSecrets ? value : "••••••••"}
      </span>
    );
  };

  return (
    <Dialog open={isOpen} onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="max-w-lg" zIndex="top">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldAlert className="h-5 w-5 text-amber-500" />
            {t("mcp.approval.title")}
          </DialogTitle>
          <DialogDescription>
            {t("mcp.approval.description", {
              name: server.name || server.id,
              app,
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="max-h-72 overflow-y-auto rounded-lg border border-border-default p-3">
          {rows.length === 0 && secretMaps.length === 0 && (
            <p className="text-xs text-muted-foreground">
              {t("mcp.approval.emptyConfig")}
            </p>
          )}
          {rows.length > 0 && (
            <dl className="space-y-1.5">
              {rows.map(({ label, value }) => (
                <div key={label} className="flex gap-2 text-xs">
                  <dt className="w-24 flex-shrink-0 text-muted-foreground">
                    {label}
                  </dt>
                  <dd className="break-all font-mono">{value}</dd>
                </div>
              ))}
            </dl>
          )}

          {secretMaps.map(({ label, entries }) => (
            <div key={label} className="mt-3">
              <p className="text-xs font-medium text-muted-foreground">
                {label}
              </p>
              <dl className="mt-1 space-y-1.5">
                {entries.map(([key, value]) => (
                  <div key={key} className="flex gap-2 text-xs">
                    <dt className="w-24 flex-shrink-0 break-all text-muted-foreground">
                      {key}
                      {isSensitiveKey(key) && (
                        <span className="ml-1 text-amber-600 dark:text-amber-400">
                          ({t("mcp.approval.sensitive")})
                        </span>
                      )}
                    </dt>
                    <dd>{renderValue(String(value))}</dd>
                  </div>
                ))}
              </dl>
            </div>
          ))}
        </div>

        <div className="flex items-center justify-between">
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            {revealSecrets ? (
              <Eye className="h-3.5 w-3.5" />
            ) : (
              <EyeOff className="h-3.5 w-3.5" />
            )}
            {t("mcp.approval.revealSecrets")}
            <Switch
              checked={revealSecrets}
              onCheckedChange={setRevealSecrets}
              aria-label={t("mcp.approval.revealSecrets")}
            />
          </label>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button onClick={onConfirm} disabled={pending}>
            {t("mcp.approval.approveAndEnable")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

export default McpApprovalDialog;
