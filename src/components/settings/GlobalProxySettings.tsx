/**
 * 全局出站代理设置组件（D10）
 *
 * 仅配置代理 URL，供 WebDAV / S3 / Skills 下载等出站请求使用；
 * 这不是本地路由。出于安全考虑不支持 URL 内嵌 `user:pass@` 认证（S10），
 * 后端 setter 会拒绝含 userinfo 的 URL。
 */

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loader2, TestTube2 } from "lucide-react";
import {
  useGlobalProxyUrl,
  useSetGlobalProxyUrl,
  useTestProxy,
} from "@/hooks/useGlobalProxy";

export function GlobalProxySettings() {
  const { t } = useTranslation();
  const { data: savedUrl, isLoading } = useGlobalProxyUrl();
  const setMutation = useSetGlobalProxyUrl();
  const testMutation = useTestProxy();

  const [url, setUrl] = useState("");
  const [dirty, setDirty] = useState(false);

  // 同步远程配置
  useEffect(() => {
    if (savedUrl !== undefined) {
      setUrl(savedUrl || "");
      setDirty(false);
    }
  }, [savedUrl]);

  const handleSave = async () => {
    await setMutation.mutateAsync(url.trim());
    setDirty(false);
  };

  const handleTest = async () => {
    if (url.trim()) {
      await testMutation.mutateAsync(url.trim());
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && dirty && !setMutation.isPending) {
      handleSave();
    }
  };

  // 只在首次加载且无数据时显示加载状态
  if (isLoading && savedUrl === undefined) {
    return (
      <div className="flex items-center justify-center p-4">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="flex gap-2">
      <Input
        placeholder="http://127.0.0.1:7890 / socks5://127.0.0.1:1080"
        value={url}
        onChange={(e) => {
          setUrl(e.target.value);
          setDirty(true);
        }}
        onKeyDown={handleKeyDown}
        className="font-mono text-sm flex-1"
      />
      <Button
        variant="outline"
        size="icon"
        disabled={!url || testMutation.isPending}
        onClick={handleTest}
        title={t("settings.globalProxy.test")}
      >
        {testMutation.isPending ? (
          <Loader2 className="h-4 w-4 animate-spin" />
        ) : (
          <TestTube2 className="h-4 w-4" />
        )}
      </Button>
      <Button
        onClick={handleSave}
        disabled={!dirty || setMutation.isPending}
        size="sm"
      >
        {setMutation.isPending && (
          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
        )}
        {t("common.save")}
      </Button>
    </div>
  );
}
