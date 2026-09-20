import React, { useCallback, useEffect, useState } from "react";
import { Eye, EyeOff, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { providersApi, type AppId } from "@/lib/api";

/** 决策 A4：显示态在失焦或 60 秒后自动重新遮罩，防止编辑页开着离开座位。 */
const REVEAL_AUTO_MASK_MS = 60_000;

interface ApiKeyInputProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  required?: boolean;
  label?: string;
  id?: string;
  /**
   * 后端凭据状态（Provider.secretStatus.apiKey，§5.2.2 前端零密钥）。
   * present=true 时输入框一律留空、不回显任何已存值，只提示「已配置」。
   */
  configuredStatus?: { present: boolean } | null;
  /**
   * 回显目标（计划 §1.4.1）。用户点眼睛时才会读一次真实值。
   * 缺省（新建供应商表单）时眼睛退化为纯遮罩开关。
   */
  revealTarget?: { app: AppId; providerId: string } | null;
}

const ApiKeyInput: React.FC<ApiKeyInputProps> = ({
  value,
  onChange,
  placeholder,
  disabled = false,
  required = false,
  label = "API Key",
  id = "apiKey",
  configuredStatus = null,
  revealTarget = null,
}) => {
  const { t } = useTranslation();
  const [showKey, setShowKey] = useState(false);
  // 零密钥要求：已配置时不回显存量值。即便上层误把密钥传进 value，
  // 在用户真正编辑前也强制显示为空（后端已剥离配置，正常路径 value 即为 ""）。
  const [userEdited, setUserEdited] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [revealError, setRevealError] = useState<string | null>(null);
  const configured = configuredStatus?.present === true;
  const pendingReveal = configured && !userEdited;
  const displayValue = pendingReveal ? "" : value;

  // 决策 A4：显示态自动重新遮罩。
  useEffect(() => {
    if (!showKey) return;
    const timer = setTimeout(() => setShowKey(false), REVEAL_AUTO_MASK_MS);
    return () => clearTimeout(timer);
  }, [showKey]);

  /**
   * §1.4.4：眼睛按钮在「已配置但未回显」时按需读一次真实值，读到的值灌进现有受控
   * 状态后，行为与「用户自己输入了这个值」完全一致——再点即切换遮罩，不改就保存
   * 等于同值覆盖。
   */
  const handleRevealClick = useCallback(async () => {
    if (showKey) {
      setShowKey(false);
      return;
    }
    if (!pendingReveal || !revealTarget) {
      setShowKey(true);
      return;
    }

    setRevealing(true);
    try {
      const revealed = await providersApi.revealSecret(
        revealTarget.app,
        revealTarget.providerId,
        "api_key",
      );
      if (revealed === null) {
        // 条目其实已经不在凭据管理器里：只提示，不动 present 状态。
        setRevealError(t("apiKeyInput.revealFailed"));
        return;
      }
      setRevealError(null);
      setUserEdited(true);
      onChange(revealed);
      setShowKey(true);
    } catch {
      setRevealError(t("apiKeyInput.revealFailed"));
    } finally {
      setRevealing(false);
    }
  }, [showKey, pendingReveal, revealTarget, onChange, t]);

  const toggleShowKey = () => {
    void handleRevealClick();
  };

  const eyeLabel = showKey
    ? t("apiKeyInput.hide")
    : pendingReveal
      ? t("apiKeyInput.reveal")
      : t("apiKeyInput.show");

  const inputClass = `w-full px-3 py-2 pr-10 border rounded-lg text-sm transition-colors ${
    disabled
      ? "bg-muted border-border-default text-muted-foreground cursor-not-allowed"
      : "border-border-default bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-blue-500/20 dark:focus:ring-blue-400/20"
  }`;

  return (
    <div className="space-y-2">
      <label htmlFor={id} className="block text-sm font-medium text-foreground">
        {label} {required && "*"}
      </label>
      <div className="relative">
        <input
          type={showKey ? "text" : "password"}
          id={id}
          value={displayValue}
          onChange={(e) => {
            setUserEdited(true);
            setRevealError(null);
            onChange(e.target.value);
          }}
          onBlur={() => {
            if (showKey) setShowKey(false);
          }}
          placeholder={
            pendingReveal
              ? "••••••••"
              : (placeholder ?? t("apiKeyInput.placeholder"))
          }
          disabled={disabled}
          required={required}
          autoComplete="off"
          className={inputClass}
        />
        {!disabled && (
          <button
            type="button"
            onClick={toggleShowKey}
            disabled={revealing}
            className="absolute inset-y-0 right-0 flex items-center pr-3 text-muted-foreground hover:text-foreground transition-colors disabled:opacity-60"
            aria-label={eyeLabel}
          >
            {revealing ? (
              <Loader2 size={16} className="animate-spin" />
            ) : showKey ? (
              <EyeOff size={16} />
            ) : (
              <Eye size={16} />
            )}
          </button>
        )}
      </div>
      {configured && (
        <p className="text-xs text-muted-foreground">
          {t("providerForm.apiKeyConfiguredHint")}
        </p>
      )}
      {revealError && <p className="text-xs text-red-500">{revealError}</p>}
    </div>
  );
};

export default ApiKeyInput;
