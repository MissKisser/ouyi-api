import { useContext } from 'react';
import { AuthContext } from '@/lib/auth-context';

/**
 * 读取认证上下文的 Hook：返回 token、login / setup / logout 与 isAuthenticated。
 * 必须在 AuthProvider 包裹的组件树内调用，否则抛出 Error。
 */
export function useAuth() {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error('useAuth must be used within AuthProvider');
  return ctx;
}
