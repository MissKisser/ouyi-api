import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { useAuth } from '@/lib/use-auth';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { KeyRound, Shield } from 'lucide-react';
import { useTranslation } from 'react-i18next';

/**
 * 登录页：挂载时以探测密码请求 POST /admin/api/login 判断后端是否已设置管理密码
 * （403 视为需要初始化设置，401 视为进入登录模式），据此渲染"设置密码（含确认框，
 * 最少 6 位）"或"输入密码登录"两种表单。提交成功后跳转首页；
 * 失败时在表单内展示错误信息。副作用：发起登录/设置请求。
 */
export function LoginPage() {
  const { t } = useTranslation();
  const { login, setup } = useAuth();
  const navigate = useNavigate();
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [needsSetup, setNeedsSetup] = useState<boolean | null>(null);

  // Check if setup is needed by trying login endpoint
  useEffect(() => {
    (async () => {
      try {
        const res = await fetch('/admin/api/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ password: '__check__' }),
        });
        await res.json().catch(() => ({}));
        // 403 = "未设置密码" → needs setup
        // 401 = "密码错误" → login mode
        setNeedsSetup(res.status === 403);
      } catch {
        setNeedsSetup(false);
      }
    })();
  }, []);

  const handleSetup = async (e: React.FormEvent) => {
    e.preventDefault();
    setError('');
    if (password.length < 6) {
      setError(t('login.errorPasswordLength'));
      return;
    }
    if (password !== confirmPassword) {
      setError(t('login.errorPasswordMismatch'));
      return;
    }
    setLoading(true);
    try {
      const result = await setup(password);
      if (result.success) {
        navigate('/', { replace: true });
      } else {
        setError(result.error || t('login.errorSetupFailed'));
      }
    } catch {
      setError(t('login.networkError'));
    } finally {
      setLoading(false);
    }
  };

  const handleLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setError('');
    setLoading(true);
    try {
      const result = await login(password);
      if (result.success) {
        navigate('/', { replace: true });
      } else {
        setError(result.error || t('login.errorLoginFailed'));
      }
    } catch {
      setError(t('login.networkError'));
    } finally {
      setLoading(false);
    }
  };

  if (needsSetup === null) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <p className="text-muted-foreground">{t('login.checking')}</p>
      </div>
    );
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          <div className="mx-auto mb-2 flex h-12 w-12 items-center justify-center rounded-full bg-primary/10">
            {needsSetup ? (
              <Shield className="h-6 w-6 text-primary" />
            ) : (
              <KeyRound className="h-6 w-6 text-primary" />
            )}
          </div>
          <CardTitle className="text-2xl">{t('login.title')}</CardTitle>
          <CardDescription>
            {needsSetup ? t('login.setupDescription') : t('login.loginDescription')}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {needsSetup ? (
            <form onSubmit={handleSetup} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="password">{t('login.setPasswordLabel')}</Label>
                <Input
                  id="password"
                  type="password"
                  placeholder={t('login.setPasswordPlaceholder')}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  disabled={loading}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="confirm">{t('login.confirmPasswordLabel')}</Label>
                <Input
                  id="confirm"
                  type="password"
                  placeholder={t('login.confirmPasswordPlaceholder')}
                  value={confirmPassword}
                  onChange={(e) => setConfirmPassword(e.target.value)}
                  disabled={loading}
                />
              </div>
              {error && <p className="text-sm text-destructive">{error}</p>}
              <Button type="submit" className="w-full" disabled={loading || !password}>
                {loading ? t('login.settingUp') : t('login.setupButton')}
              </Button>
            </form>
          ) : (
            <form onSubmit={handleLogin} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="password">{t('login.passwordLabel')}</Label>
                <Input
                  id="password"
                  type="password"
                  placeholder={t('login.passwordPlaceholder')}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  disabled={loading}
                />
              </div>
              {error && <p className="text-sm text-destructive">{error}</p>}
              <Button type="submit" className="w-full" disabled={loading || !password}>
                {loading ? t('login.verifying') : t('login.loginButton')}
              </Button>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
