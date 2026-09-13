import { useMemo } from "react";
import type { DraggableAttributes } from "@dnd-kit/core";
import type { DraggableSyntheticListeners } from "@dnd-kit/core";
import { GripVertical } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Provider } from "@/types";
import type { AppId } from "@/lib/api";
import { cn } from "@/lib/utils";
import { ProviderActions } from "@/components/providers/ProviderActions";
import { ProviderIcon } from "@/components/ProviderIcon";
import { extractCodexBaseUrl } from "@/utils/providerConfigUtils";
import { resolveCodexOfficialIdentity } from "@/utils/providerCapabilities";
import { ProviderStatusBadge } from "@/components/providers/ProviderStatusBadge";
import { resolveProviderIcon } from "@/utils/providerIcon";

interface DragHandleProps {
  attributes: DraggableAttributes;
  listeners: DraggableSyntheticListeners;
  isDragging: boolean;
}

interface ProviderCardProps {
  provider: Provider;
  isCurrent: boolean;
  appId: AppId;
  isInConfig?: boolean; // Pi: 是否已添加到 pi 配置
  onSwitch: (provider: Provider) => void;
  onEdit: (provider: Provider) => void;
  onDelete: (provider: Provider) => void;
  onRemoveFromConfig?: (provider: Provider) => void;
  onOpenWebsite: (url: string) => void;
  onDuplicate: (provider: Provider) => void;
  onTest?: (provider: Provider) => void;
  onOpenTerminal?: (provider: Provider) => void;
  isTesting?: boolean;
  dragHandleProps?: DragHandleProps;
  isRemovalProtected?: boolean;
  isStateChangeProtected?: boolean;
}

const extractApiUrl = (provider: Provider, fallbackText: string) => {
  if (provider.notes?.trim()) {
    return provider.notes.trim();
  }

  if (provider.websiteUrl) {
    return provider.websiteUrl;
  }

  const config = provider.settingsConfig;

  if (config && typeof config === "object") {
    const object = config as Record<string, any>;
    const envBase = object?.env?.ANTHROPIC_BASE_URL;
    if (typeof envBase === "string" && envBase.trim()) {
      return envBase;
    }

    const directBaseUrl = object.baseUrl || object.base_url;
    if (typeof directBaseUrl === "string" && directBaseUrl.trim()) {
      return directBaseUrl;
    }

    const baseUrl = object.config;

    if (typeof baseUrl === "string" && baseUrl.includes("base_url")) {
      const extractedBaseUrl = extractCodexBaseUrl(baseUrl);
      if (extractedBaseUrl) {
        return extractedBaseUrl;
      }
    }
  }

  return fallbackText;
};

export function ProviderCard({
  provider,
  isCurrent,
  appId,
  isInConfig = true,
  onSwitch,
  onEdit,
  onDelete,
  onRemoveFromConfig,
  onOpenWebsite,
  onDuplicate,
  onTest,
  onOpenTerminal,
  isTesting,
  dragHandleProps,
  isRemovalProtected,
  isStateChangeProtected,
}: ProviderCardProps) {
  const { t } = useTranslation();
  const codexOfficialIdentity = resolveCodexOfficialIdentity(appId, provider);
  const manualNote = provider.notes?.trim() || undefined;

  const isAdditiveMode = appId === "pi";

  const fallbackUrlText = t("provider.notConfigured", {
    defaultValue: "未配置接口地址",
  });

  const displayUrl = useMemo(() => {
    return extractApiUrl(provider, fallbackUrlText);
  }, [provider, fallbackUrlText]);

  const isClickableUrl = useMemo(() => {
    if (provider.notes?.trim()) {
      return false;
    }
    if (displayUrl === fallbackUrlText) {
      return false;
    }
    return true;
  }, [provider.notes, displayUrl, fallbackUrlText]);

  const handleOpenWebsite = () => {
    if (!isClickableUrl) {
      return;
    }
    onOpenWebsite(displayUrl);
  };

  // 判断是否是"当前使用中"的供应商
  // - Pi：使用 isInConfig 代替 isCurrent（累加模式）
  // - 普通模式：isCurrent
  const isActiveProvider = isAdditiveMode ? Boolean(isInConfig) : isCurrent;

  const hasPersistentConfigHighlight = isAdditiveMode && isInConfig;
  const shouldUseBlue = isActiveProvider || hasPersistentConfigHighlight;
  const hasStateHighlight = shouldUseBlue;

  return (
    <div
      className={cn(
        "relative overflow-hidden rounded-xl border border-border p-4 transition-all duration-300",
        "bg-card text-card-foreground group",
        "hover:border-border-active",
        shouldUseBlue && "border-blue-500/60 shadow-sm shadow-blue-500/10",
        !hasStateHighlight && "hover:shadow-sm",
        dragHandleProps?.isDragging &&
          "cursor-grabbing border-primary shadow-lg scale-105 z-10",
      )}
    >
      <div
        className={cn(
          "absolute inset-0 bg-gradient-to-r to-transparent transition-opacity duration-500 pointer-events-none",
          shouldUseBlue && "from-blue-500/10",
          !hasStateHighlight && "from-primary/10",
          hasStateHighlight ? "opacity-100" : "opacity-0",
        )}
      />
      <div className="relative flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          {dragHandleProps && (
            <button
              type="button"
              className={cn(
                "-ml-1.5 flex-shrink-0 cursor-grab active:cursor-grabbing p-1.5",
                "text-muted-foreground/50 hover:text-muted-foreground transition-colors",
                dragHandleProps.isDragging && "cursor-grabbing",
              )}
              aria-label={t("provider.dragHandle")}
              {...dragHandleProps.attributes}
              {...dragHandleProps.listeners}
            >
              <GripVertical className="h-4 w-4" />
            </button>
          )}

          <div className="h-8 w-8 flex-shrink-0 rounded-lg bg-muted flex items-center justify-center border border-border group-hover:scale-105 transition-transform duration-300">
            <ProviderIcon
              icon={resolveProviderIcon(
                appId,
                provider.icon,
                provider.iconColor,
              )}
              name={provider.name}
              color={provider.iconColor}
              size={20}
            />
          </div>

          <div className="min-w-0 flex-1 space-y-1">
            <div className="flex flex-wrap items-center gap-2 min-h-7">
              <h3
                className={cn(
                  "text-base font-semibold leading-none",
                  codexOfficialIdentity && "min-w-0 flex-1 truncate",
                )}
                title={codexOfficialIdentity ? provider.name : undefined}
              >
                {provider.name}
              </h3>

              {appId === "claude" && provider.category === "official" && (
                <ProviderStatusBadge
                  label={t("provider.noRoutingSupport", {
                    defaultValue: "不支持路由",
                  })}
                />
              )}
            </div>

            {codexOfficialIdentity === "native_login" ? (
              <div className="flex min-w-0 items-center gap-2 text-sm text-muted-foreground">
                <span className="min-w-0 truncate" title={manualNote}>
                  {manualNote ??
                    t("codex.followCodexLoginDescription", {
                      defaultValue: "账号会随 Codex CLI 当前登录变化",
                    })}
                </span>
              </div>
            ) : displayUrl ? (
              <button
                type="button"
                onClick={handleOpenWebsite}
                className={cn(
                  "inline-flex max-w-full items-center overflow-hidden text-left text-sm",
                  isClickableUrl
                    ? "text-blue-500 transition-colors hover:underline dark:text-blue-400 cursor-pointer"
                    : "text-muted-foreground cursor-default",
                )}
                title={displayUrl}
                disabled={!isClickableUrl}
              >
                <span className="min-w-0 truncate">{displayUrl}</span>
              </button>
            ) : null}
          </div>
        </div>

        <div className="flex items-center ml-auto min-w-0 gap-3">
          <div className="flex items-center gap-1.5 flex-shrink-0 opacity-0 pointer-events-none group-hover:opacity-100 group-focus-within:opacity-100 group-hover:pointer-events-auto group-focus-within:pointer-events-auto transition-opacity duration-200">
            <ProviderActions
              appId={appId}
              isCurrent={isCurrent}
              isInConfig={isInConfig}
              isTesting={isTesting}
              onSwitch={() => onSwitch(provider)}
              onEdit={() => onEdit(provider)}
              onDuplicate={() => onDuplicate(provider)}
              onTest={
                // 连通检测对第三方/自定义供应商开放，官方供应商
                // (category === "official") 一律隐藏：它们 base_url 故意留空、
                // 走客户端默认/OAuth 端点，cc-switch 没有可靠的探测目标
                onTest && provider.category !== "official"
                  ? () => onTest(provider)
                  : undefined
              }
              onDelete={() => onDelete(provider)}
              onRemoveFromConfig={
                onRemoveFromConfig
                  ? () => onRemoveFromConfig(provider)
                  : undefined
              }
              onOpenTerminal={
                onOpenTerminal ? () => onOpenTerminal(provider) : undefined
              }
              isRemovalProtected={isRemovalProtected}
              isStateChangeProtected={isStateChangeProtected}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
