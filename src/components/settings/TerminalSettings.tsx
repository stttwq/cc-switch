import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import { isMac, isWindows, isLinux } from "@/lib/platform";

// Terminal options per platform
const MACOS_TERMINALS = [
  { value: "terminal", labelKey: "settings.terminal.options.macos.terminal" },
  { value: "iterm2", labelKey: "settings.terminal.options.macos.iterm2" },
  { value: "alacritty", labelKey: "settings.terminal.options.macos.alacritty" },
  { value: "kitty", labelKey: "settings.terminal.options.macos.kitty" },
  { value: "ghostty", labelKey: "settings.terminal.options.macos.ghostty" },
  { value: "otty", labelKey: "settings.terminal.options.macos.otty" },
  { value: "wezterm", labelKey: "settings.terminal.options.macos.wezterm" },
  { value: "kaku", labelKey: "settings.terminal.options.macos.kaku" },
  { value: "warp", labelKey: "settings.terminal.options.macos.warp" },
] as const;

const WINDOWS_TERMINALS = [
  { value: "cmd", labelKey: "settings.terminal.options.windows.cmd" },
  {
    value: "powershell",
    labelKey: "settings.terminal.options.windows.powershell",
  },
  { value: "wt", labelKey: "settings.terminal.options.windows.wt" },
  { value: "custom", labelKey: "settings.terminal.options.windows.custom" },
] as const;

const LINUX_TERMINALS = [
  {
    value: "gnome-terminal",
    labelKey: "settings.terminal.options.linux.gnomeTerminal",
  },
  { value: "konsole", labelKey: "settings.terminal.options.linux.konsole" },
  {
    value: "xfce4-terminal",
    labelKey: "settings.terminal.options.linux.xfce4Terminal",
  },
  { value: "alacritty", labelKey: "settings.terminal.options.linux.alacritty" },
  { value: "kitty", labelKey: "settings.terminal.options.linux.kitty" },
  { value: "ghostty", labelKey: "settings.terminal.options.linux.ghostty" },
] as const;

// Get terminals for the current platform
function getTerminalOptions() {
  if (isMac()) {
    return MACOS_TERMINALS;
  }
  if (isWindows()) {
    return WINDOWS_TERMINALS;
  }
  if (isLinux()) {
    return LINUX_TERMINALS;
  }
  // Fallback to macOS options
  return MACOS_TERMINALS;
}

// Get default terminal for the current platform
function getDefaultTerminal(): string {
  if (isMac()) {
    return "terminal";
  }
  if (isWindows()) {
    return "cmd";
  }
  if (isLinux()) {
    return "gnome-terminal";
  }
  return "terminal";
}

export interface TerminalSettingsProps {
  value?: string;
  customPath?: string;
  customArgs?: string;
  onChange: (value: string) => void;
  onCustomChange: (updates: {
    preferredTerminalCustomPath?: string;
    preferredTerminalCustomArgs?: string;
  }) => void;
}

export function TerminalSettings({
  value,
  customPath,
  customArgs,
  onChange,
  onCustomChange,
}: TerminalSettingsProps) {
  const { t } = useTranslation();
  const terminals = getTerminalOptions();
  const defaultTerminal = getDefaultTerminal();

  // Use value or default
  const currentValue = value || defaultTerminal;
  const isCustom = currentValue === "custom";

  // 输入框本地态：onBlur 才落盘，避免每个按键都触发一次设置保存。
  const [pathDraft, setPathDraft] = useState(customPath ?? "");
  const [argsDraft, setArgsDraft] = useState(customArgs ?? "");
  const [pathDirty, setPathDirty] = useState(false);
  const [argsDirty, setArgsDirty] = useState(false);

  useEffect(() => {
    if (!pathDirty) setPathDraft(customPath ?? "");
  }, [customPath, pathDirty]);

  useEffect(() => {
    if (!argsDirty) setArgsDraft(customArgs ?? "");
  }, [customArgs, argsDirty]);

  return (
    <section className="space-y-2">
      <header className="space-y-1">
        <h3 className="text-sm font-medium">{t("settings.terminal.title")}</h3>
        <p className="text-xs text-muted-foreground">
          {t("settings.terminal.description")}
        </p>
      </header>
      <Select value={currentValue} onValueChange={onChange}>
        <SelectTrigger className="w-[200px]">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {terminals.map((terminal) => (
            <SelectItem key={terminal.value} value={terminal.value}>
              {t(terminal.labelKey)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {isCustom && (
        <div className="space-y-2 pt-1">
          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">
              {t("settings.terminal.customPathLabel")}
            </label>
            <Input
              className="w-[360px]"
              placeholder={t("settings.terminal.customPathPlaceholder")}
              value={pathDraft}
              onChange={(e) => {
                setPathDraft(e.target.value);
                setPathDirty(true);
              }}
              onBlur={() => {
                if (pathDirty) {
                  onCustomChange({ preferredTerminalCustomPath: pathDraft });
                  setPathDirty(false);
                }
              }}
            />
          </div>
          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">
              {t("settings.terminal.customArgsLabel")}
            </label>
            <Input
              className="w-[360px]"
              placeholder='-e cmd /K "{bat}"'
              value={argsDraft}
              onChange={(e) => {
                setArgsDraft(e.target.value);
                setArgsDirty(true);
              }}
              onBlur={() => {
                if (argsDirty) {
                  onCustomChange({ preferredTerminalCustomArgs: argsDraft });
                  setArgsDirty(false);
                }
              }}
            />
            <p className="text-xs text-muted-foreground">
              {t("settings.terminal.customArgsHint")}
            </p>
          </div>
        </div>
      )}
      <p className="text-xs text-muted-foreground">
        {t("settings.terminal.fallbackHint")}
      </p>
    </section>
  );
}
