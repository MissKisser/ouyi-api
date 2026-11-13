import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

/**
 * 合并 className：先用 clsx 拼接各类值（字符串/数组/对象条件类名），
 * 再用 tailwind-merge 解决冲突的 Tailwind 工具类（后者覆盖前者）。
 * @param inputs 任意数量的类名输入
 * @returns 合并去重后的类名字符串
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
