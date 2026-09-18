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
  /** 来源类型: "system" 表示系统环境变量, "file" 表示配置文件 */
  sourceType: "system" | "file";
  /** 来源路径 (注册表路径或文件路径:行号) */
  sourcePath: string;
}
