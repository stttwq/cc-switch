import { useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  envDeliveryAdopt,
  type DeliveryConflict,
  type EnvConflictPrompt,
} from "@/lib/api/env";
import { extractErrorMessage } from "@/utils/errorUtils";

interface EnvConflictDialogProps {
  open: boolean;
  /** 触发冲突的上下文（应用 / 目标供应商 / 冲突列表 / 重试闭包） */
  prompt: EnvConflictPrompt;
}

/**
 * 受控的环境变量冲突对话框（§5.3.3）：切换供应商撞上来历不明的同名变量时，
 * 让用户在「接管（覆盖）并继续切换」与「取消切换」之间显式选择。
 *
 * 列表只渲染每个冲突的 name 与 maskedValue（末 4 位）——后端契约
 * DeliveryConflict 只含 name/owner/maskedValue，本就没有完整值，组件也只
 * 读取 maskedValue，永不显示原值。接管后原值被覆盖且不保留，事先明确告知。
 */
export function EnvConflictDialog({ open, prompt }: EnvConflictDialogProps) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const { app, providerId, conflicts, retry, onAdopted, onCancel } = prompt;

  const handleAdopt = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await envDeliveryAdopt(
        app,
        providerId,
        conflicts.map((conflict: DeliveryConflict) => conflict.name),
      );
      // 接管成功：先让宿主清空提示，再重跑一次原切换
      onAdopted();
      retry();
    } catch (error) {
      toast.error(t("envConflict.adoptFailed"), {
        description: extractErrorMessage(error) || t("common.unknown"),
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) onCancel();
      }}
    >
      <DialogContent className="max-w-md" zIndex="top">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <AlertTriangle className="h-5 w-5 text-destructive" />
            {t("envConflict.title")}
          </DialogTitle>
          <DialogDescription>
            {t("envConflict.overwriteNotice")}
          </DialogDescription>
        </DialogHeader>
        <div className="px-6 py-4">
          <ul className="text-sm">
            <li className="flex items-center justify-between gap-6 border-b border-border-default py-1.5 font-medium text-foreground">
              <span>{t("envConflict.colName")}</span>
              <span className="shrink-0">{t("envConflict.colHint")}</span>
            </li>
            {conflicts.map((conflict) => (
              <li
                key={`${app}-${providerId}-${conflict.name}`}
                className="flex items-center justify-between gap-6 py-1.5"
              >
                <span className="break-all text-foreground">
                  {conflict.name}
                </span>
                <span className="shrink-0 text-muted-foreground">
                  {conflict.maskedValue}
                </span>
              </li>
            ))}
          </ul>
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={onCancel}>
            {t("envConflict.cancel")}
          </Button>
          <Button disabled={busy} onClick={() => void handleAdopt()}>
            {t("envConflict.adopt")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
