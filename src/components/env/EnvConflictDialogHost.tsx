import { useSyncExternalStore } from "react";
import {
  getEnvConflictPromptSnapshot,
  subscribeEnvConflictPrompt,
} from "@/lib/api/env";
import { EnvConflictDialog } from "./EnvConflictDialog";

/**
 * 冲突对话框宿主：订阅 lib/api/env.ts 里的模块级订阅桥。
 * useSwitchProviderMutation 在切换失败解析到
 * ENV_CONFLICT 时 publishEnvConflictPrompt()；这里消费并渲染
 * EnvConflictDialog，让用户在「接管并切换」与「取消切换」之间显式选择
 * （§5.3.3）。挂载一次即可，见 main.tsx 中 <App /> 之后。
 */
export function EnvConflictDialogHost() {
  const prompt = useSyncExternalStore(
    subscribeEnvConflictPrompt,
    getEnvConflictPromptSnapshot,
  );
  if (!prompt) return null;
  return <EnvConflictDialog open prompt={prompt} />;
}
