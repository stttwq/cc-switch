/**
 * 环境变量冲突检测相关类型定义
 */

/**
 * 环境变量冲突信息
 */
export interface EnvConflict {
  /** 环境变量名称 */
  varName: string;
  /** 脱敏后的值（末 4 位） */
  maskedValue: string;
  /** 来源类型: "system" 系统环境变量, "file" 配置文件, "claude_settings_local" Claude 本地设置 */
  sourceType: "system" | "file" | "claude_settings_local";
  /** 来源路径 (注册表路径或文件路径:行号) */
  sourcePath: string;
  /** 只读告警：cc-switch 不代改该来源，只展示、不提供删除（§5.3.1 注） */
  readOnly?: boolean;
}
