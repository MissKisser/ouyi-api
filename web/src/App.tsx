import { Routes, Route, Navigate } from 'react-router-dom';
import { AuthProvider } from '@/lib/auth';
import { useAuth } from '@/lib/use-auth';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { Layout } from '@/pages/Layout';
import { LoginPage } from '@/pages/LoginPage';
import { DashboardPage } from '@/pages/DashboardPage';
import { ModelsPage } from '@/pages/ModelsPage';
import { ConfigPage } from '@/pages/ConfigPage';
import { LogsPage } from '@/pages/LogsPage';
import { ChatTestPage } from '@/pages/ChatTestPage';
import { ApiEndpointsPage } from '@/pages/ApiEndpointsPage';

/**
 * 受保护路由容器：未登录时重定向到 /login，已登录时在 Layout 下渲染全部管理页面。
 * 返回包含仪表盘、模型、配置、日志、对话测试、端点列表等子路由的 <Routes> 结构。
 */
function ProtectedRoutes() {
  const { isAuthenticated } = useAuth();
  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<DashboardPage />} />
        <Route path="models" element={<ModelsPage />} />
        <Route path="config" element={<ConfigPage />} />
        <Route path="logs" element={<LogsPage />} />
        <Route path="chat" element={<ChatTestPage />} />
        <Route path="endpoints" element={<ApiEndpointsPage />} />
      </Route>
    </Routes>
  );
}

/**
 * 应用根组件：包一层 AuthProvider 提供登录态，再以 ErrorBoundary 兜底渲染错误，
 * 并挂载 /login 路由与受保护路由（/*）。作为路由默认导出，由 main.tsx 渲染。
 */
function App() {
  return (
    <AuthProvider>
      <ErrorBoundary>
        <Routes>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/*" element={<ProtectedRoutes />} />
        </Routes>
      </ErrorBoundary>
    </AuthProvider>
  );
}

export default App;
