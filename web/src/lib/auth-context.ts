import { createContext } from 'react';

/** 认证上下文暴露给消费方的数据与操作集合。 */
interface AuthContextType {
  /** 当前登录 token；未登录时为 null。 */
  token: string | null;
  /**
   * 用密码登录：调用后端登录接口，成功时把 token 写入 localStorage 并更新状态。
   * 永不抛出异常，失败以 error 文本返回。
   */
  login: (password: string) => Promise<{ success: boolean; error?: string }>;
  /**
   * 首次初始化设置管理密码：成功时同样持久化 token 并更新状态。
   * 永不抛出异常，失败以 error 文本返回。
   */
  setup: (password: string) => Promise<{ success: boolean; error?: string }>;
  /** 登出：清空 localStorage 中的 token 并重置状态。 */
  logout: () => void;
  /** 是否已登录（token 非空即视为已认证）。 */
  isAuthenticated: boolean;
}

/**
 * 认证上下文：由 AuthProvider 提供值，未包裹在 Provider 内使用时值为 null。
 * 消费方应通过 useAuth() 读取而非直接 useContext。
 */
export const AuthContext = createContext<AuthContextType | null>(null);
/** 认证上下文的类型定义，供需要显式标注类型的地方引用。 */
export type { AuthContextType };
