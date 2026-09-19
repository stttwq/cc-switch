import React, { useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import { useTranslation } from "react-i18next";

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
}) => {
  const { t } = useTranslation();
  const [showKey, setShowKey] = useState(false);
  // 零密钥要求：已配置时不回显存量值。即便上层误把密钥传进 value，
  // 在用户真正编辑前也强制显示为空（后端已剥离配置，正常路径 value 即为 ""）。
  const [userEdited, setUserEdited] = useState(false);
  const configured = configuredStatus?.present === true;
  const displayValue = configured && !userEdited ? "" : value;

  const toggleShowKey = () => {
    setShowKey(!showKey);
  };

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
            onChange(e.target.value);
          }}
          placeholder={placeholder ?? t("apiKeyInput.placeholder")}
          disabled={disabled}
          required={required}
          autoComplete="off"
          className={inputClass}
        />
        {!disabled && displayValue && (
          <button
            type="button"
            onClick={toggleShowKey}
            className="absolute inset-y-0 right-0 flex items-center pr-3 text-muted-foreground hover:text-foreground transition-colors"
            aria-label={showKey ? t("apiKeyInput.hide") : t("apiKeyInput.show")}
          >
            {showKey ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        )}
      </div>
      {configured && (
        <p className="text-xs text-muted-foreground">
          {t("providerForm.apiKeyConfigured")}
        </p>
      )}
    </div>
  );
};

export default ApiKeyInput;
