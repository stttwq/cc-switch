import { useCallback, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { FullScreenPanel } from "@/components/common/FullScreenPanel";
import type { Provider } from "@/types";
import type { AppId } from "@/lib/api";
import {
  ProviderForm,
  type ProviderFormValues,
} from "@/components/providers/forms/ProviderForm";

interface AddProviderDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  appId: AppId;
  onSubmit: (
    provider: Omit<Provider, "id"> & {
      providerKey?: string;
    },
  ) => Promise<void> | void;
}

export function AddProviderDialog({
  open,
  onOpenChange,
  appId,
  onSubmit,
}: AddProviderDialogProps) {
  const { t } = useTranslation();
  const [isFormSubmitting, setIsFormSubmitting] = useState(false);

  const closeDialog = useCallback(() => {
    onOpenChange(false);
  }, [onOpenChange]);

  const handlePanelClose = useCallback(() => {
    closeDialog();
  }, [closeDialog]);
  const formReadyToken = useMemo(
    () => Symbol("provider-form-ready"),
    [appId, open],
  );
  const currentFormReadyToken = useRef(formReadyToken);
  currentFormReadyToken.current = formReadyToken;
  const [formReadyState, setFormReadyState] = useState({
    token: formReadyToken,
    ready: appId !== "pi",
  });
  const isFormReady =
    formReadyState.token === formReadyToken
      ? formReadyState.ready
      : appId !== "pi";
  const handleSubmitReadyChange = useCallback(
    (ready: boolean) => {
      if (currentFormReadyToken.current === formReadyToken) {
        setFormReadyState({ token: formReadyToken, ready });
      }
    },
    [formReadyToken],
  );

  const handleSubmit = useCallback(
    async (values: ProviderFormValues) => {
      const parsedConfig = JSON.parse(values.settingsConfig) as Record<
        string,
        unknown
      >;

      // 构造基础提交数据
      const providerData: Omit<Provider, "id"> & {
        providerKey?: string;
      } = {
        name: values.name.trim(),
        notes: values.notes?.trim() || undefined,
        websiteUrl: values.websiteUrl?.trim() || undefined,
        settingsConfig: parsedConfig,
        icon: values.icon?.trim() || undefined,
        iconColor: values.iconColor?.trim() || undefined,
        ...(values.presetCategory ? { category: values.presetCategory } : {}),
        ...(values.meta ? { meta: values.meta } : {}),
      };

      // Pi 的原生配置有稳定的 provider key，用它作为管理的供应商标识。
      if (appId === "pi" && values.providerKey) {
        providerData.providerKey = values.providerKey;
      }

      await onSubmit(providerData);
      closeDialog();
    },
    [appId, onSubmit, closeDialog],
  );

  const footer = (
    <>
      <span className="mr-auto min-w-0 text-xs text-muted-foreground truncate">
        {t("provider.addFooterHint")}
      </span>
      <Button
        variant="outline"
        onClick={closeDialog}
        className="border-border/20 hover:bg-accent hover:text-accent-foreground"
      >
        {t("common.cancel")}
      </Button>
      <Button
        type="submit"
        form="provider-form"
        disabled={isFormSubmitting || !isFormReady}
        className="bg-primary text-primary-foreground hover:bg-primary/90"
      >
        {isFormSubmitting ? (
          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
        ) : (
          <Plus className="mr-2 h-4 w-4" />
        )}
        {t("common.add")}
      </Button>
    </>
  );

  return (
    <FullScreenPanel
      isOpen={open}
      title={t("provider.addNewProvider")}
      onClose={handlePanelClose}
      footer={footer}
      contentClassName={appId === "pi" ? "pt-3 pb-0" : "pt-3"}
    >
      <ProviderForm
        appId={appId}
        submitLabel={t("common.add")}
        onSubmit={handleSubmit}
        onCancel={closeDialog}
        onSubmittingChange={setIsFormSubmitting}
        onSubmitReadyChange={handleSubmitReadyChange}
        showButtons={false}
      />
    </FullScreenPanel>
  );
}
