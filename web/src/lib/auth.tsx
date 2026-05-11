import { useState, useCallback, useEffect } from 'react';
import { AuthContext } from '@/lib/auth-context';
import { getToken, setToken, clearToken, apiLogin, apiSetup, ApiError, setOnUnauthorized } from '@/lib/api';

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [token, setTokenState] = useState<string | null>(getToken());

  const login = useCallback(async (password: string): Promise<{ success: boolean; error?: string }> => {
    try {
      const res = await apiLogin(password);
      setToken(res.token);
      setTokenState(res.token);
      return { success: true };
    } catch (e) {
      const msg = e instanceof ApiError ? e.message : '登录失败';
      return { success: false, error: msg };
    }
  }, []);

  const setup = useCallback(async (password: string): Promise<{ success: boolean; error?: string }> => {
    try {
      const res = await apiSetup(password);
      setToken(res.token);
      setTokenState(res.token);
      return { success: true };
    } catch (e) {
      const msg = e instanceof ApiError ? e.message : '设置失败';
      return { success: false, error: msg };
    }
  }, []);

  const logout = useCallback(() => {
    clearToken();
    setTokenState(null);
  }, []);
  // 注册 401 回调：当 API 收到 401 时自动同步 token 状态，触发跳转 login
  useEffect(() => {
    setOnUnauthorized(() => setTokenState(null));
    return () => setOnUnauthorized(null);
  }, []);

  return (
    <AuthContext.Provider value={{ token, login, setup, logout, isAuthenticated: token !== null }}>
      {children}
    </AuthContext.Provider>
  );
}
