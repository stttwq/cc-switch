import {
  Activity,
  Check,
  Copy,
  Edit,
  Loader2,
  Minus,
  Play,
  Plus,
  Terminal,
  Trash2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { AppId } from "@/lib/api";
import { isAdditiveAppId } from "@/config/appConfig";

interface ProviderActionsProps {
  appId?: AppId;
  isCurrent: boolean;
  isInConfig?: boolean;
  isTesting?: boolean;
  onSwitch: () => void;
  onEdit: () => void;
  onDuplicate?: () => void;
  onTest?: () => void;
  onDelete: () => void;
  onRemoveFromConfig?: () => void;
  onOpenTerminal?: () => void;
  // Hermes v12+ providers: dict overlay — edit/delete must go through Web UI
  isReadOnly?: boolean;
  isRemovalProtected?: boolean;
  isStateChangeProtected?: boolean;
}

// 主按钮的呈现状态。title 用于 disabled 态向用户解释为何不可点击；
// 因 Button 基类带 disabled:pointer-events-none，title 必须挂在外层非禁用
// 的 wrapper 上才会在 hover 时显示（见下方 <span> 包裹）。
interface MainButtonState {
  disabled: boolean;
  variant: "default" | "secondary";
  className: string;
  icon: JSX.Element;
  text: string;
  title?: string;
}

export function ProviderActions({
  appId,
  isCurrent,
  isInConfig = false,
  isTesting,
  onSwitch,
  onEdit,
  onDuplicate,
  onTest,
  onDelete,
  onRemoveFromConfig,
  onOpenTerminal,
  isReadOnly = false,
  isRemovalProtected = false,
  isStateChangeProtected = false,
}: ProviderActionsProps) {
  const { t } = useTranslation();
  const iconButtonClass = "h-8 w-8 p-1";

  // Additive provider membership: providers can coexist in the native config.
  const isAdditiveMode = Boolean(appId && isAdditiveAppId(appId));
  const isMembershipMode = isAdditiveMode;
  const piStateChangeHint = t("pi.current.stateUnavailableHint");

  const handleMainButtonClick = () => {
    if (isMembershipMode) {
      // 累加模式：切换配置状态（添加/移除）
      if (isInConfig) {
        if (onRemoveFromConfig) {
          onRemoveFromConfig();
        } else {
          onDelete();
        }
      } else {
        onSwitch(); // 添加到配置
      }
    } else {
      onSwitch();
    }
  };

  const getMainButtonState = (): MainButtonState => {
    // 累加模式（Pi）
    if (isMembershipMode) {
      if (isStateChangeProtected) {
        return {
          disabled: true,
          variant: "secondary" as const,
          className: "opacity-40 cursor-not-allowed",
          icon: isInConfig ? (
            <Minus className="h-4 w-4" />
          ) : (
            <Plus className="h-4 w-4" />
          ),
          text: isInConfig
            ? t("provider.removeFromConfig", { defaultValue: "移除" })
            : t("provider.enable", { defaultValue: "启用" }),
          title: piStateChangeHint,
        };
      }
      if (isInConfig) {
        return {
          disabled: isRemovalProtected,
          variant: "secondary" as const,
          className: cn(
            "bg-orange-100 text-orange-600 hover:bg-orange-200 dark:bg-orange-900/50 dark:text-orange-400 dark:hover:bg-orange-900/70",
            isRemovalProtected && "opacity-40 cursor-not-allowed",
          ),
          icon: <Minus className="h-4 w-4" />,
          text: t("provider.removeFromConfig", { defaultValue: "移除" }),
        };
      }
      return {
        disabled: false,
        variant: "default" as const,
        className:
          "bg-emerald-500 hover:bg-emerald-600 dark:bg-emerald-600 dark:hover:bg-emerald-700",
        icon: <Plus className="h-4 w-4" />,
        text:
          appId === "pi"
            ? t("provider.enable", { defaultValue: "启用" })
            : t("provider.addToConfig", { defaultValue: "添加" }),
      };
    }

    if (isCurrent) {
      return {
        disabled: true,
        variant: "secondary" as const,
        className:
          "bg-gray-200 text-muted-foreground hover:bg-gray-200 hover:text-muted-foreground dark:bg-gray-700 dark:hover:bg-gray-700",
        icon: <Check className="h-4 w-4" />,
        text: t("provider.inUse"),
      };
    }

    return {
      disabled: false,
      variant: "default" as const,
      className: "",
      icon: <Play className="h-4 w-4" />,
      text: t("provider.enable"),
    };
  };

  const buttonState = getMainButtonState();
  const canDelete =
    !isReadOnly && (appId === "pi" ? !isStateChangeProtected : true);
  const readOnlyHint = t("provider.managedByHermes", {
    defaultValue: "由 Hermes 管理，请在 Hermes Web UI 中编辑",
  });
  const deleteHint =
    appId === "pi" && isStateChangeProtected
      ? piStateChangeHint
      : isReadOnly
        ? readOnlyHint
        : t("common.delete");

  return (
    <div className="flex items-center gap-1.5">
      {/* disabled:pointer-events-none prevents the native title from firing,
          so the wrapper owns the explanatory tooltip and cursor. */}
      <span
        title={buttonState.title}
        className={cn(
          "inline-flex",
          buttonState.disabled && "cursor-not-allowed",
        )}
      >
        <Button
          size="sm"
          variant={buttonState.variant}
          onClick={handleMainButtonClick}
          disabled={buttonState.disabled}
          className={cn("w-[4.5rem] px-2.5", buttonState.className)}
        >
          {buttonState.icon}
          {buttonState.text}
        </Button>
      </span>

      <div className="flex items-center gap-1">
        <Button
          size="icon"
          variant="ghost"
          onClick={isReadOnly ? undefined : onEdit}
          disabled={isReadOnly}
          aria-label={t("common.edit")}
          title={isReadOnly ? readOnlyHint : t("common.edit")}
          className={cn(
            iconButtonClass,
            isReadOnly && "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          <Edit className="h-4 w-4" />
        </Button>

        {onDuplicate && (
          <Button
            size="icon"
            variant="ghost"
            onClick={onDuplicate}
            title={t("provider.duplicate")}
            className={iconButtonClass}
          >
            <Copy className="h-4 w-4" />
          </Button>
        )}

        <Button
          size="icon"
          variant="ghost"
          onClick={onTest || undefined}
          disabled={isTesting}
          title={t("provider.connectivityCheck", "检测连通")}
          className={cn(
            iconButtonClass,
            !onTest && "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          {isTesting ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Activity className="h-4 w-4" />
          )}
        </Button>

        {onOpenTerminal && (
          <Button
            size="icon"
            variant="ghost"
            onClick={onOpenTerminal}
            title={t("provider.openTerminal", "打开终端")}
            className={cn(
              iconButtonClass,
              "hover:text-emerald-600 dark:hover:text-emerald-400",
            )}
          >
            <Terminal className="h-4 w-4" />
          </Button>
        )}

        <Button
          size="icon"
          variant="ghost"
          onClick={canDelete ? onDelete : undefined}
          disabled={!canDelete}
          aria-label={t("common.delete")}
          title={deleteHint}
          className={cn(
            iconButtonClass,
            canDelete && "hover:text-red-500 dark:hover:text-red-400",
            !canDelete && "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
