import { useMemo } from "react";
import type { DraggableAttributes } from "@dnd-kit/core";
import type { DraggableSyntheticListeners } from "@dnd-kit/core";
import { GripVertical } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Provider } from "@/types";
import type { AppId } from "@/lib/api";
import { cn } from "@/lib/utils";
import { useSettingsQuery } from "@/lib/query";
import { ProviderActions } from "@/components/providers/ProviderActions";
import { ProviderIcon } from "@/components/ProviderIcon";
import { extractCodexBaseUrl } from "@/utils/providerConfigUtils";
import { resolveCodexOfficialIdentity } from "@/utils/providerCapabilities";

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
  onOpenTerminal?: (provider: Provider) => void;
  onRunCli?: (provider: Provider) => void;
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

/**
 * §1.4.5：卡片上显示当前端点的主机名，让用户不进编辑页也知道切到了哪个端点。
 *
 * 只取 `secretStatus.baseUrl`——它由后端从凭据管理器读回，是唯一权威来源；
 * settingsConfig 里的 base URL 已被提取器剥离，不能作为依据。
 */
const extractEndpointHost = (provider: Provider): string | null => {
  const raw = provider.secretStatus?.baseUrl?.trim();
  if (!raw) return null;
  try {
    return new URL(raw).host || null;
  } catch {
    // 不是合法 URL（例如用户填了裸主机名）时原样显示，总比不显示好。
    return raw;
  }
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
  onOpenTerminal,
  onRunCli,
  dragHandleProps,
  isRemovalProtected,
  isStateChangeProtected,
}: ProviderCardProps) {
  const { t } = useTranslation();
  const settingsData = useSettingsQuery().data;
  // P2 分级：徽章按该卡片所属 app 的有效严格性显示（全局，或该 app 在按应用列表里）。
  const envStrictMode =
    (settingsData?.envDeliveryStrictMode ?? false) ||
    (settingsData?.envDeliveryStrictApps ?? []).includes(appId);
  const codexOfficialIdentity = resolveCodexOfficialIdentity(appId, provider);
  const manualNote = provider.notes?.trim() || undefined;

  const isAdditiveMode = appId === "pi";

  const fallbackUrlText = t("provider.notConfigured", {
    defaultValue: "未配置接口地址",
  });

  const displayUrl = useMemo(() => {
    return extractApiUrl(provider, fallbackUrlText);
  }, [provider, fallbackUrlText]);

  const endpointHost = useMemo(() => extractEndpointHost(provider), [provider]);

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
              icon={provider.icon}
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
              {/* §5.5：密钥不随同步/导出走，跨机还原后要重新输入才能切换 */}
              {provider.secretStatus &&
                !provider.secretStatus.apiKey.present &&
                provider.category !== "official" &&
                !codexOfficialIdentity && (
                  <span
                    className="rounded-md bg-amber-500/15 px-1.5 py-0.5 text-xs text-amber-600 dark:text-amber-400"
                    title={t("provider.keyMissingHint")}
                  >
                    {t("provider.keyMissing")}
                  </span>
                )}
              {/* B5 严格投递模式：密钥不落 HKCU\Environment，只经 cc-switch「打开终端」注入 */}
              {envStrictMode && (
                <span
                  className="rounded-md bg-rose-500/15 px-1.5 py-0.5 text-xs text-rose-600 dark:text-rose-400"
                  title={t("provider.strictDeliveryHint")}
                >
                  {t("provider.strictDelivery")}
                </span>
              )}
              {/* §1.4.5：端点主机名（完整 URL 在 title 里） */}
              {endpointHost && (
                <span
                  className="max-w-[16rem] truncate rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground"
                  title={`${t("provider.endpoint")}: ${provider.secretStatus?.baseUrl}`}
                >
                  {endpointHost}
                </span>
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
              onSwitch={() => onSwitch(provider)}
              onEdit={() => onEdit(provider)}
              onDuplicate={() => onDuplicate(provider)}
              onDelete={() => onDelete(provider)}
              onRemoveFromConfig={
                onRemoveFromConfig
                  ? () => onRemoveFromConfig(provider)
                  : undefined
              }
              onOpenTerminal={
                onOpenTerminal ? () => onOpenTerminal(provider) : undefined
              }
              onRunCli={onRunCli ? () => onRunCli(provider) : undefined}
              isRemovalProtected={isRemovalProtected}
              isStateChangeProtected={isStateChangeProtected}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
