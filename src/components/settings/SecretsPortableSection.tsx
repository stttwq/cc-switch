import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Download, Loader2, Upload } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/** 后端 `secrets::portable::MIN_PASSPHRASE_CHARS` 的镜像，仅用于表单即时提示。 */
const MIN_PASSPHRASE_CHARS = 20;

interface ExportResult {
  exported: number;
  appSecrets: number;
  filePath: string;
}

interface ImportResult {
  imported: number;
  overwritten: number;
  unchanged: number;
  appSecrets: number;
}

/**
 * 凭据便携包：把凭据管理器里的 `cc-switch/*` 条目加密导出成一个文件，换机时导入。
 *
 * 存在的理由：凭据不参与 WebDAV/S3 同步（「同步不包含密钥」），而卸载清理会删掉
 * 本机全部 `cc-switch/*` 条目 —— 两者相加就是「卸载 + 重装 + 同步 → 密钥永久丢失」。
 * 这条路径由用户显式操作，用独立口令，密文落盘。
 */
export function SecretsPortableSection() {
  const { t } = useTranslation();
  const [passphrase, setPassphrase] = useState("");
  const [busy, setBusy] = useState<"export" | "import" | null>(null);
  const [backend, setBackend] = useState<string>("windows");

  useEffect(() => {
    invoke<string>("secret_backend_name")
      .then(setBackend)
      .catch(() => setBackend("windows"));
  }, []);
  // D9：1Password 自带跨设备同步，导出=把钥匙搬出保险箱 → 隐藏导出，保留导入。
  const isOnePassword = backend === "onepassword";

  const tooShort =
    passphrase.length > 0 && passphrase.length < MIN_PASSPHRASE_CHARS;

  const runExport = async () => {
    if (passphrase.length < MIN_PASSPHRASE_CHARS) {
      toast.error(
        t("secretsPortable.passphraseTooShort", { min: MIN_PASSPHRASE_CHARS }),
      );
      return;
    }
    setBusy("export");
    try {
      const result = await invoke<ExportResult | null>(
        "secrets_export_via_dialog",
        { passphrase },
      );
      // 取消对话框返回 null，不是失败。
      if (result) {
        toast.success(
          t("secretsPortable.exportDone", {
            count: result.exported,
            path: result.filePath,
          }),
        );
      }
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(null);
    }
  };

  const runImport = async () => {
    if (passphrase.length < MIN_PASSPHRASE_CHARS) {
      toast.error(
        t("secretsPortable.passphraseTooShort", { min: MIN_PASSPHRASE_CHARS }),
      );
      return;
    }
    setBusy("import");
    try {
      const result = await invoke<ImportResult | null>(
        "secrets_import_via_dialog",
        { passphrase },
      );
      if (result) {
        toast.success(
          t("secretsPortable.importDone", {
            imported: result.imported,
            overwritten: result.overwritten,
            unchanged: result.unchanged,
          }),
        );
      }
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {t("secretsPortable.hint")}
      </p>

      <div className="space-y-2">
        <label className="text-sm font-medium" htmlFor="secrets-portable-pass">
          {t("secretsPortable.passphraseLabel", { min: MIN_PASSPHRASE_CHARS })}
        </label>
        <Input
          id="secrets-portable-pass"
          type="password"
          autoComplete="new-password"
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
          placeholder={t("secretsPortable.passphrasePlaceholder")}
        />
        <p
          className={
            tooShort
              ? "text-xs text-destructive"
              : "text-xs text-muted-foreground"
          }
        >
          {tooShort
            ? t("secretsPortable.passphraseTooShort", {
                min: MIN_PASSPHRASE_CHARS,
              })
            : t("secretsPortable.passphraseHint", {
                min: MIN_PASSPHRASE_CHARS,
              })}
        </p>
      </div>

      <div className="flex flex-wrap gap-2">
        {!isOnePassword && (
          <Button
            variant="outline"
            size="sm"
            disabled={busy !== null}
            onClick={runExport}
          >
            {busy === "export" ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <Download className="mr-2 h-4 w-4" />
            )}
            {t("secretsPortable.export")}
          </Button>
        )}
        <Button
          variant="outline"
          size="sm"
          disabled={busy !== null}
          onClick={runImport}
        >
          {busy === "import" ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Upload className="mr-2 h-4 w-4" />
          )}
          {t("secretsPortable.import")}
        </Button>
      </div>
    </div>
  );
}
