import { NavLink, Outlet, useNavigate } from 'react-router-dom';
import { useAuth } from '@/lib/use-auth';
import { Button } from '@/components/ui/button';
import { Separator } from '@/components/ui/separator';
import { LanguageSwitcher } from '@/components/LanguageSwitcher';
import {
  LayoutDashboard,
  Boxes,
  Settings,
  LogOut,
  ScrollText,
  MessageSquare,
  Link2,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 管理端整体布局：左侧固定侧边栏（Logo、导航菜单、语言切换与登出），
 * 右侧为可滚动主内容区（渲染嵌套路由 <Outlet />）。
 * 登出时清空认证状态并跳转 /login；导航项高亮当前路由。
 */
export function Layout() {
  const { t } = useTranslation();
  const { logout } = useAuth();
  const navigate = useNavigate();

  const handleLogout = () => {
    logout();
    navigate('/login');
  };

  const navItems = [
    { to: '/', icon: LayoutDashboard, label: t('nav.dashboard') },
    { to: '/chat', icon: MessageSquare, label: t('nav.chat') },
    { to: '/models', icon: Boxes, label: t('nav.models') },
    { to: '/endpoints', icon: Link2, label: t('nav.endpoints') },
    { to: '/config', icon: Settings, label: t('nav.config') },
    { to: '/logs', icon: ScrollText, label: t('nav.logs') },
  ];

  return (
    <div className="h-screen flex bg-background overflow-hidden">
      {/* Sidebar — fixed at viewport height, never scrolls with main */}
      <aside className="w-56 shrink-0 border-r bg-card flex flex-col h-full overflow-y-auto">
        <div className="p-4 flex items-center gap-2 shrink-0">
          <img src={`${import.meta.env.BASE_URL}admin/favicon.svg`} alt="Logo" className="h-6 w-6" />
          <span className="font-bold text-lg">DS Free API</span>
        </div>
        <Separator className="shrink-0" />
        <nav className="flex-1 p-2 space-y-1 min-h-0">
          {navItems.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                `flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors ${
                  isActive
                    ? 'bg-primary/10 text-primary'
                    : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
                }`
              }
            >
              <Icon className="h-4 w-4" />
              {label}
            </NavLink>
          ))}
        </nav>
        <Separator className="shrink-0" />
        <div className="p-2 space-y-2 shrink-0">
          <LanguageSwitcher />
          <Button
            variant="ghost"
            className="w-full justify-start gap-3 text-muted-foreground"
            onClick={handleLogout}
          >
            <LogOut className="h-4 w-4" />
            {t('nav.logout')}
          </Button>
        </div>
      </aside>

      {/* Main content — owns the scroll, never grows the document */}
      <main className="flex-1 min-w-0 h-full overflow-auto">
        <div className="p-6 w-full">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
