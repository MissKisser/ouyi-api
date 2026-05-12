import { useEffect, useState } from 'react';
import {
  apiFetchConfig,
  apiSaveConfig,
  apiTestToolRuntime,
  type FullConfig,
  type AccountEntry,
  type ToolRuntimeTestResponse,
} from '@/lib/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { Separator } from '@/components/ui/separator';
import { Switch } from '@/components/ui/switch';
import {
  ChevronDown,
  ChevronRight,
  Copy,
  Eye,
  EyeOff,
  Plus,
  Save,
  PlugZap,
  X,
  Server,
  Wrench,
  User,
  Shield,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';

/** Collapsible section wrapper */
function Section({
  title,
  icon: Icon,
  defaultOpen = false,
  children,
}: {
  title: string;
  icon: React.ElementType;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <Card>
      <CardHeader className="cursor-pointer select-none" onClick={() => setOpen(!open)}>
        <CardTitle className="flex items-center gap-2 text-lg">
          <Icon className="h-5 w-5" />
          {title}
          <span className="ml-auto text-muted-foreground">
            {open ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
          </span>
        </CardTitle>
      </CardHeader>
      {open && <CardContent>{children}</CardContent>}
    </Card>
  );
}

export function ConfigPage() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<FullConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ type: 'ok' | 'err'; text: string } | null>(null);
  const [oldPassword, setOldPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [showToolApiKey, setShowToolApiKey] = useState(false);
  const [testingToolRuntime, setTestingToolRuntime] = useState(false);
  const [toolRuntimeTest, setToolRuntimeTest] = useState<ToolRuntimeTestResponse | null>(null);

  const loadConfig = () => {
    apiFetchConfig()
      .then(setConfig)
      .catch(() => setMessage({ type: 'err', text: t('config.loadFailed') }));
  };

  useEffect(loadConfig, [t]);

  if (!config) {
    return <div className="p-4 text-muted-foreground">{t('config.loading')}</div>;
  }

  const update = <T,>(path: string[], value: T) => {
    setConfig((prev) => {
      if (!prev) return prev;
      const next = structuredClone(prev) as unknown as Record<string, unknown>;
      let obj: Record<string, unknown> = next;
      for (let i = 0; i < path.length - 1; i++) {
        obj = obj[path[i]] as Record<string, unknown>;
      }
      obj[path[path.length - 1]] = value as unknown;
      return next as unknown as FullConfig;
    });
  };

  const handleSave = async () => {
    setSaving(true);
    setMessage(null);
    try {
      const body: Record<string, unknown> = {
        server: {
          host: config.server.host,
          port: config.server.port,
        },
        admin: {
          old_password: oldPassword || undefined,
          new_password: newPassword || undefined,
        },
        accounts: config.accounts.map((a) => ({
          email: a.email,
          mobile: a.mobile,
          area_code: a.area_code,
          password: a.password,
          token: a.token,
          enabled: a.enabled,
        })),
        tool_runtime: config.tool_runtime,
      };
      await apiSaveConfig(body);
      setMessage({ type: 'ok', text: t('config.saveSuccess') });
      setOldPassword('');
      setNewPassword('');
      const fresh = await apiFetchConfig();
      setConfig(fresh);
    } catch (e: unknown) {
      setMessage({
        type: 'err',
        text: t('config.saveFailed', { error: e instanceof Error ? e.message : String(e) }),
      });
    } finally {
      setSaving(false);
    }
  };

  const handleCancel = () => {
    if (confirm(t('config.cancelConfirm'))) {
      loadConfig();
    }
  };

  const handleToolRuntimeTest = async () => {
    if (!config.tool_runtime) return;
    setTestingToolRuntime(true);
    setToolRuntimeTest(null);
    try {
      const result = await apiTestToolRuntime(config.tool_runtime);
      setToolRuntimeTest(result);
    } catch (e: unknown) {
      setToolRuntimeTest({
        success: false,
        message: e instanceof Error ? e.message : String(e),
        latency_ms: 0,
        mode: 'compiler',
        provider: config.tool_runtime.provider,
        model: config.tool_runtime.model,
        base_url: config.tool_runtime.base_url,
      });
    } finally {
      setTestingToolRuntime(false);
    }
  };

  const copyToClipboard = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const el = document.createElement('textarea');
      el.value = text;
      document.body.appendChild(el);
      el.select();
      document.execCommand('copy');
      document.body.removeChild(el);
    }
  };

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">{t('config.title')}</h1>

      {message && (
        <div
          className={`p-3 rounded-md text-sm ${
            message.type === 'err' ? 'bg-red-50 text-red-700' : 'bg-green-50 text-green-700'
          }`}
        >
          {message.text}
        </div>
      )}

      {/* Accounts */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-lg">
            <User className="h-5 w-5" /> {t('config.sections.accounts')}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {config.accounts.map((a, i) => (
            <div key={i} className="flex flex-wrap items-end gap-2 p-3 border rounded-md">
              <div className="flex-1 min-w-[120px]">
                <label className="text-xs text-muted-foreground">{t('config.accounts.email')}</label>
                <Input
                  value={a.email}
                  onChange={(e) => {
                    const next = [...config.accounts];
                    next[i] = { ...next[i], email: e.target.value };
                    update(['accounts'], next);
                  }}
                />
              </div>
              <div className="w-24">
                <label className="text-xs text-muted-foreground">{t('config.accounts.mobile')}</label>
                <Input
                  value={a.mobile}
                  onChange={(e) => {
                    const next = [...config.accounts];
                    next[i] = { ...next[i], mobile: e.target.value };
                    update(['accounts'], next);
                  }}
                />
              </div>
              <div className="w-20">
                <label className="text-xs text-muted-foreground">{t('config.accounts.areaCode')}</label>
                <Input
                  value={a.area_code}
                  onChange={(e) => {
                    const next = [...config.accounts];
                    next[i] = { ...next[i], area_code: e.target.value };
                    update(['accounts'], next);
                  }}
                />
              </div>
              <div className="flex-1 min-w-[120px]">
                <label className="text-xs text-muted-foreground">{t('config.accounts.token')}</label>
                <div className="flex items-center gap-1">
                  <Input type="password" value={a.token || ''} readOnly className="bg-muted/50" />
                  <Button
                    variant="ghost"
                    size="icon"
                    className="shrink-0"
                    onClick={() => a.token && copyToClipboard(a.token)}
                    disabled={!a.token}
                  >
                    <Copy className="h-4 w-4" />
                  </Button>
                </div>
              </div>
              <div className="flex items-center gap-2 pb-1">
                <label className="text-xs text-muted-foreground">{t('config.accounts.enabled')}</label>
                <Switch
                  checked={a.enabled}
                  onCheckedChange={(checked) => {
                    const next = [...config.accounts];
                    next[i] = { ...next[i], enabled: checked };
                    update(['accounts'], next);
                  }}
                />
              </div>
              <Button
                variant="ghost"
                size="icon"
                className="shrink-0"
                onClick={() => update(['accounts'], config.accounts.filter((_, j) => j !== i))}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          ))}
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              update(['accounts'], [
                ...config.accounts,
                {
                  email: '',
                  mobile: '',
                  area_code: '',
                  password: '',
                  token: '',
                  enabled: true,
                } as AccountEntry,
              ])
            }
          >
            <Plus className="h-4 w-4 mr-1" /> {t('config.accounts.add')}
          </Button>
        </CardContent>
      </Card>

      {/* Tool Runtime */}
      <Section title={t('config.sections.toolRuntime')} icon={Wrench}>
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <label className="text-sm text-muted-foreground">{t('config.toolRuntime.enabled')}</label>
              <Switch
                checked={config.tool_runtime?.enabled ?? false}
                onCheckedChange={(checked) => {
                  const current = config.tool_runtime ?? {
                    enabled: false,
                    provider: 'openai-compatible',
                    base_url: '',
                    api_key: '',
                    model: '',
                    timeout_secs: 30,
                  };
                  update(['tool_runtime'], { ...current, enabled: checked });
                  setToolRuntimeTest(null);
                }}
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={handleToolRuntimeTest}
              disabled={testingToolRuntime || !(config.tool_runtime?.enabled ?? false)}
            >
              <PlugZap className="h-4 w-4 mr-1" />
              {testingToolRuntime ? t('config.toolRuntime.testing') : t('config.toolRuntime.testConnection')}
            </Button>
          </div>
          {(config.tool_runtime?.enabled ?? false) && (
            <>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">
                    {t('config.toolRuntime.provider')}
                  </label>
                  <select
                    className="w-full border rounded-md p-2"
                    value={config.tool_runtime?.provider ?? 'openai-compatible'}
                    onChange={(e) => {
                      update(['tool_runtime', 'provider'], e.target.value);
                      setToolRuntimeTest(null);
                    }}
                  >
                    <option value="anthropic">{t('config.toolRuntime.providerAnthropic')}</option>
                    <option value="openai-compatible">{t('config.toolRuntime.providerOpenaiCompatible')}</option>
                  </select>
                </div>
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">
                    {t('config.toolRuntime.baseUrl')}
                  </label>
                  <Input
                    value={config.tool_runtime?.base_url ?? ''}
                    onChange={(e) => {
                      update(['tool_runtime', 'base_url'], e.target.value);
                      setToolRuntimeTest(null);
                    }}
                    placeholder="https://api.example.com/v1"
                  />
                </div>
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.model')}</label>
                  <Input
                    value={config.tool_runtime?.model ?? ''}
                    onChange={(e) => {
                      update(['tool_runtime', 'model'], e.target.value);
                      setToolRuntimeTest(null);
                    }}
                    placeholder="cheap-tool-model"
                  />
                </div>
                <div className="md:col-span-2">
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.apiKey')}</label>
                  <div className="flex items-center gap-1">
                    <Input
                      type={showToolApiKey ? 'text' : 'password'}
                      value={config.tool_runtime?.api_key ?? ''}
                      onChange={(e) => {
                        update(['tool_runtime', 'api_key'], e.target.value);
                        setToolRuntimeTest(null);
                      }}
                      className="font-mono text-xs"
                    />
                    <Button
                      variant="ghost"
                      size="icon"
                      className="shrink-0"
                      onClick={() => setShowToolApiKey(!showToolApiKey)}
                    >
                      {showToolApiKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="shrink-0"
                      onClick={() => config.tool_runtime?.api_key && copyToClipboard(config.tool_runtime.api_key)}
                      disabled={!config.tool_runtime?.api_key}
                    >
                      <Copy className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.timeout')}</label>
                  <Input
                    type="number"
                    value={config.tool_runtime?.timeout_secs ?? 30}
                    onChange={(e) => {
                      update(['tool_runtime', 'timeout_secs'], Number(e.target.value));
                      setToolRuntimeTest(null);
                    }}
                  />
                </div>
              </div>
              {toolRuntimeTest && (
                <div
                  className={`p-3 rounded-md text-sm ${
                    toolRuntimeTest.success ? 'bg-green-50 text-green-700' : 'bg-red-50 text-red-700'
                  }`}
                >
                  <div className="font-medium">{toolRuntimeTest.message}</div>
                  <div className="mt-1 text-xs opacity-80">
                    {toolRuntimeTest.provider} · {toolRuntimeTest.model || '-'} · {toolRuntimeTest.latency_ms}ms
                  </div>
                </div>
              )}
            </>
          )}
        </div>
      </Section>

      {/* Server */}
      <Section title={t('config.sections.server')} icon={Server}>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.server.host')}</label>
            <Input value={config.server.host} onChange={(e) => update(['server', 'host'], e.target.value)} />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.server.port')}</label>
            <Input
              type="number"
              value={config.server.port}
              onChange={(e) => update(['server', 'port'], Number(e.target.value))}
            />
          </div>
        </div>
      </Section>

      {/* Admin */}
      <Section title="Admin" icon={Shield}>
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <Badge variant={config.admin.password_set ? 'default' : 'secondary'}>
              {config.admin.password_set ? t('config.admin.passwordSet') : t('config.admin.passwordNotSet')}
            </Badge>
          </div>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div>
              <label className="text-sm text-muted-foreground block mb-1">{t('config.admin.oldPassword')}</label>
              <Input
                type="password"
                value={oldPassword}
                onChange={(e) => setOldPassword(e.target.value)}
                placeholder={t('config.admin.oldPasswordPlaceholder')}
              />
            </div>
            <div>
              <label className="text-sm text-muted-foreground block mb-1">{t('config.admin.newPassword')}</label>
              <Input
                type="password"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                placeholder={t('config.admin.newPasswordPlaceholder')}
              />
            </div>
          </div>
        </div>
      </Section>

      <Separator className="my-2" />

      {/* Action buttons */}
      <div className="flex justify-end gap-3">
        <Button variant="outline" onClick={handleCancel} disabled={saving}>
          {t('config.cancel')}
        </Button>
        <Button onClick={handleSave} disabled={saving}>
          <Save className="h-5 w-5 mr-2" />
          {saving ? t('config.saving') : t('config.save')}
        </Button>
      </div>
    </div>
  );
}
