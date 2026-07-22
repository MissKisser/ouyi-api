import { useEffect, useState } from 'react';
import {
  apiFetchConfig,
  apiSaveConfig,
  apiSaveConcurrencyLimit,
  apiTestToolRuntime,
  apiTestOpenAiToolRuntime,
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
  Globe,
  Gauge,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import {
  MAX_CONCURRENT_REQUESTS,
  isValidMaxConcurrentRequests,
  parseMaxConcurrentRequestsInput,
} from '@/lib/concurrency';

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
  const [persistedConcurrency, setPersistedConcurrency] = useState<number | null>(null);
  const [savingConcurrency, setSavingConcurrency] = useState(false);
  const [concurrencyMessage, setConcurrencyMessage] = useState<{ type: 'ok' | 'err'; text: string } | null>(null);
  const [message, setMessage] = useState<{ type: 'ok' | 'err'; text: string } | null>(null);
  const [oldPassword, setOldPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [showToolApiKey, setShowToolApiKey] = useState(false);
  const [showOpenAiToolApiKey, setShowOpenAiToolApiKey] = useState(false);
  const [testingToolRuntime, setTestingToolRuntime] = useState(false);
  const [testingOpenAiToolRuntime, setTestingOpenAiToolRuntime] = useState(false);
  const [toolRuntimeTest, setToolRuntimeTest] = useState<ToolRuntimeTestResponse | null>(null);
  const [openAiToolRuntimeTest, setOpenAiToolRuntimeTest] = useState<ToolRuntimeTestResponse | null>(null);

  const defaultPlatform = config?.platform ?? {
    default: 'ouyi8' as const,
    prompt_on_start: true,
  };
  const defaultOuyi7 = config?.ouyi_v7 ?? {
    base_url: 'https://api-8.rcouyi.com',
    chatapi_prefix: '/chatapi',
    jwt_token: '',
    default_model: 'ouyi-chat',
    model_refresh_interval_secs: 1800,
    max_history_messages: 20,
    simulated_stream_chunk_chars: 80,
  };

  const loadConfig = () => {
    apiFetchConfig()
      .then((fresh) => {
        setConfig(fresh);
        setPersistedConcurrency(fresh.limits.max_concurrent_requests);
        setConcurrencyMessage(null);
      })
      .catch(() => setMessage({ type: 'err', text: t('config.loadFailed') }));
  };

  useEffect(loadConfig, [t]);

  if (!config) {
    return <div className="p-4 text-muted-foreground">{t('config.loading')}</div>;
  }

  const valid = isValidMaxConcurrentRequests(config.limits.max_concurrent_requests);
  const concurrencyDirty =
    persistedConcurrency !== null && persistedConcurrency !== config.limits.max_concurrent_requests;

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
    if (!valid) {
      setMessage({ type: 'err', text: t('config.server.maxConcurrentRequestsError') });
      return;
    }

    setSaving(true);
    setMessage(null);
    try {
      const body: Record<string, unknown> = {
        server: {
          host: config.server.host,
          port: config.server.port,
        },
        limits: config.limits,
        deepseek: config.deepseek,
        platform: config.platform,
        ouyi_v7: config.ouyi_v7,
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
        openai_tool_runtime: config.openai_tool_runtime,
      };
      await apiSaveConfig(body);
      setMessage({ type: 'ok', text: t('config.saveSuccess') });
      setOldPassword('');
      setNewPassword('');
      const fresh = await apiFetchConfig();
      if (fresh.limits.max_concurrent_requests !== config.limits.max_concurrent_requests) {
        throw new Error('Concurrency limit was not persisted');
      }
      setConfig(fresh);
      setPersistedConcurrency(fresh.limits.max_concurrent_requests);
    } catch (e: unknown) {
      setMessage({
        type: 'err',
        text: t('config.saveFailed', { error: e instanceof Error ? e.message : String(e) }),
      });
    } finally {
      setSaving(false);
    }
  };

  const handleSaveConcurrency = async () => {
    const value = config.limits.max_concurrent_requests;
    if (!valid) {
      setConcurrencyMessage({ type: 'err', text: t('config.server.maxConcurrentRequestsError') });
      return;
    }
    setSavingConcurrency(true);
    setConcurrencyMessage(null);
    try {
      const fresh = await apiSaveConcurrencyLimit(value);
      setConfig((prev) => (prev ? { ...prev, limits: fresh.limits } : prev));
      setPersistedConcurrency(value);
      setConcurrencyMessage({ type: 'ok', text: t('config.server.maxConcurrentRequestsSaved') });
    } catch (e: unknown) {
      setConcurrencyMessage({
        type: 'err',
        text: t('config.server.maxConcurrentRequestsSaveFailed', {
          error: e instanceof Error ? e.message : String(e),
        }),
      });
    } finally {
      setSavingConcurrency(false);
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

  const handleOpenAiToolRuntimeTest = async () => {
    if (!config.openai_tool_runtime) return;
    setTestingOpenAiToolRuntime(true);
    setOpenAiToolRuntimeTest(null);
    try {
      const result = await apiTestOpenAiToolRuntime(config.openai_tool_runtime);
      setOpenAiToolRuntimeTest(result);
    } catch (e: unknown) {
      setOpenAiToolRuntimeTest({
        success: false,
        message: e instanceof Error ? e.message : String(e),
        latency_ms: 0,
        mode: 'compiler',
        provider: config.openai_tool_runtime.provider,
        model: config.openai_tool_runtime.model,
        base_url: config.openai_tool_runtime.base_url,
      });
    } finally {
      setTestingOpenAiToolRuntime(false);
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
                  <Input
                    type="password"
                    value={a.token || ''}
                    onChange={(e) => {
                      const next = [...config.accounts];
                      next[i] = { ...next[i], token: e.target.value };
                      update(['accounts'], next);
                    }}
                    placeholder={t('config.accounts.tokenPlaceholder')}
                    className="font-mono text-xs"
                  />
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

      {/* Ouyi API */}
      <Section title={t('config.sections.ouyi')} icon={Globe} defaultOpen>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.deepseek.apiBase')}</label>
            <Input value={config.deepseek.api_base} onChange={(e) => update(['deepseek', 'api_base'], e.target.value)} />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.deepseek.clientVersion')}</label>
            <Input
              value={config.deepseek.client_version}
              onChange={(e) => update(['deepseek', 'client_version'], e.target.value)}
            />
          </div>
          <div className="md:col-span-2">
            <label className="text-sm text-muted-foreground block mb-1">{t('config.deepseek.jwtToken')}</label>
            <Input
              type="password"
              value={config.deepseek.jwt_token}
              onChange={(e) => update(['deepseek', 'jwt_token'], e.target.value)}
              className="font-mono text-xs"
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">
              {t('config.deepseek.modelRefreshInterval')}
            </label>
            <Input
              type="number"
              value={config.deepseek.model_refresh_interval_secs}
              onChange={(e) => update(['deepseek', 'model_refresh_interval_secs'], Number(e.target.value))}
            />
          </div>
        </div>
      </Section>

      {/* Concurrency limits */}
      <Section title={t('config.sections.limits')} icon={Gauge} defaultOpen>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="text-sm text-muted-foreground block mb-1">
              {t('config.server.maxConcurrentRequests')}
            </label>
            <Input
              type="number"
              min={0}
              max={MAX_CONCURRENT_REQUESTS}
              step={1}
              value={Number.isNaN(config.limits.max_concurrent_requests) ? '' : config.limits.max_concurrent_requests}
              onChange={(e) =>
                update(['limits', 'max_concurrent_requests'], parseMaxConcurrentRequestsInput(e.target.value))
              }
              aria-invalid={!valid}
            />
            <p className={`mt-1 text-xs ${valid ? 'text-muted-foreground' : 'text-destructive'}`}>
              {!valid
                ? t('config.server.maxConcurrentRequestsError')
                : config.limits.max_concurrent_requests === 0
                  ? t('config.server.maxConcurrentRequestsUnlimited')
                  : t('config.server.maxConcurrentRequestsHotReloadHint')}
            </p>
            <div className="mt-2 flex items-center gap-2">
              <Button
                type="button"
                size="sm"
                onClick={handleSaveConcurrency}
                disabled={!valid || !concurrencyDirty || savingConcurrency || saving}
              >
                <Save className="h-4 w-4 mr-1" />
                {savingConcurrency ? t('config.saving') : t('config.server.maxConcurrentRequestsSave')}
              </Button>
              {concurrencyDirty && !savingConcurrency && (
                <span className="text-xs text-amber-600">{t('config.server.maxConcurrentRequestsDirty')}</span>
              )}
            </div>
            {concurrencyMessage && (
              <p className={`mt-2 text-xs ${concurrencyMessage.type === 'err' ? 'text-destructive' : 'text-green-700'}`}>
                {concurrencyMessage.text}
              </p>
            )}
          </div>
        </div>
      </Section>

      {/* Ouyi 7 */}
      <Section title={t('config.sections.ouyi7')} icon={Globe}>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.baseUrl')}</label>
            <Input
              value={defaultOuyi7.base_url}
              onChange={(e) => update(['ouyi_v7'], { ...defaultOuyi7, base_url: e.target.value })}
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.chatapiPrefix')}</label>
            <Input
              value={defaultOuyi7.chatapi_prefix}
              onChange={(e) => update(['ouyi_v7'], { ...defaultOuyi7, chatapi_prefix: e.target.value })}
            />
          </div>
          <div className="md:col-span-2">
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.jwtToken')}</label>
            <Input
              type="password"
              value={defaultOuyi7.jwt_token}
              onChange={(e) => update(['ouyi_v7'], { ...defaultOuyi7, jwt_token: e.target.value })}
              className="font-mono text-xs"
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.defaultModel')}</label>
            <Input
              value={defaultOuyi7.default_model}
              onChange={(e) => update(['ouyi_v7'], { ...defaultOuyi7, default_model: e.target.value })}
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.modelRefreshInterval')}</label>
            <Input
              type="number"
              value={defaultOuyi7.model_refresh_interval_secs}
              onChange={(e) =>
                update(['ouyi_v7'], { ...defaultOuyi7, model_refresh_interval_secs: Number(e.target.value) })
              }
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.maxHistoryMessages')}</label>
            <Input
              type="number"
              value={defaultOuyi7.max_history_messages}
              onChange={(e) => update(['ouyi_v7'], { ...defaultOuyi7, max_history_messages: Number(e.target.value) })}
            />
          </div>
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.ouyi7.streamChunkChars')}</label>
            <Input
              type="number"
              value={defaultOuyi7.simulated_stream_chunk_chars}
              onChange={(e) =>
                update(['ouyi_v7'], { ...defaultOuyi7, simulated_stream_chunk_chars: Number(e.target.value) })
              }
            />
          </div>
        </div>
      </Section>

      {/* Platform */}
      <Section title={t('config.sections.platform')} icon={Globe}>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="text-sm text-muted-foreground block mb-1">{t('config.platform.default')}</label>
            <select
              className="w-full border rounded-md p-2"
              value={defaultPlatform.default}
              onChange={(e) => update(['platform'], { ...defaultPlatform, default: e.target.value })}
            >
              <option value="ouyi8">{t('config.platform.ouyi8')}</option>
              <option value="ouyi7">{t('config.platform.ouyi7')}</option>
            </select>
          </div>
          <div className="flex items-center gap-2 pt-6">
            <label className="text-sm text-muted-foreground">{t('config.platform.promptOnStart')}</label>
            <Switch
              checked={defaultPlatform.prompt_on_start}
              onCheckedChange={(checked) => update(['platform'], { ...defaultPlatform, prompt_on_start: checked })}
            />
          </div>
        </div>
      </Section>

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

      {/* OpenAI Tool Runtime */}
      <Section title={t('config.sections.openaiToolRuntime')} icon={Wrench}>
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <label className="text-sm text-muted-foreground">{t('config.toolRuntime.enabled')}</label>
              <Switch
                checked={config.openai_tool_runtime?.enabled ?? false}
                onCheckedChange={(checked) => {
                  const current = config.openai_tool_runtime ?? {
                    enabled: false,
                    provider: 'openai-compatible',
                    base_url: '',
                    api_key: '',
                    model: '',
                    timeout_secs: 30,
                  };
                  update(['openai_tool_runtime'], { ...current, enabled: checked, provider: 'openai-compatible' });
                  setOpenAiToolRuntimeTest(null);
                }}
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={handleOpenAiToolRuntimeTest}
              disabled={testingOpenAiToolRuntime || !(config.openai_tool_runtime?.enabled ?? false)}
            >
              <PlugZap className="h-4 w-4 mr-1" />
              {testingOpenAiToolRuntime ? t('config.toolRuntime.testing') : t('config.toolRuntime.testConnection')}
            </Button>
          </div>
          {(config.openai_tool_runtime?.enabled ?? false) && (
            <>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">
                    {t('config.toolRuntime.baseUrl')}
                  </label>
                  <Input
                    value={config.openai_tool_runtime?.base_url ?? ''}
                    onChange={(e) => {
                      update(['openai_tool_runtime', 'base_url'], e.target.value);
                      setOpenAiToolRuntimeTest(null);
                    }}
                    placeholder="https://api.example.com/v1"
                  />
                </div>
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.model')}</label>
                  <Input
                    value={config.openai_tool_runtime?.model ?? ''}
                    onChange={(e) => {
                      update(['openai_tool_runtime', 'model'], e.target.value);
                      setOpenAiToolRuntimeTest(null);
                    }}
                    placeholder="tool-model"
                  />
                </div>
                <div className="md:col-span-2">
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.apiKey')}</label>
                  <div className="flex items-center gap-1">
                    <Input
                      type={showOpenAiToolApiKey ? 'text' : 'password'}
                      value={config.openai_tool_runtime?.api_key ?? ''}
                      onChange={(e) => {
                        update(['openai_tool_runtime', 'api_key'], e.target.value);
                        setOpenAiToolRuntimeTest(null);
                      }}
                      className="font-mono text-xs"
                    />
                    <Button
                      variant="ghost"
                      size="icon"
                      className="shrink-0"
                      onClick={() => setShowOpenAiToolApiKey(!showOpenAiToolApiKey)}
                    >
                      {showOpenAiToolApiKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="shrink-0"
                      onClick={() => config.openai_tool_runtime?.api_key && copyToClipboard(config.openai_tool_runtime.api_key)}
                      disabled={!config.openai_tool_runtime?.api_key}
                    >
                      <Copy className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
                <div>
                  <label className="text-sm text-muted-foreground block mb-1">{t('config.toolRuntime.timeout')}</label>
                  <Input
                    type="number"
                    value={config.openai_tool_runtime?.timeout_secs ?? 30}
                    onChange={(e) => {
                      update(['openai_tool_runtime', 'timeout_secs'], Number(e.target.value));
                      setOpenAiToolRuntimeTest(null);
                    }}
                  />
                </div>
              </div>
              {openAiToolRuntimeTest && (
                <div
                  className={`p-3 rounded-md text-sm ${
                    openAiToolRuntimeTest.success ? 'bg-green-50 text-green-700' : 'bg-red-50 text-red-700'
                  }`}
                >
                  <div className="font-medium">{openAiToolRuntimeTest.message}</div>
                  <div className="mt-1 text-xs opacity-80">
                    {openAiToolRuntimeTest.provider} · {openAiToolRuntimeTest.model || '-'} · {openAiToolRuntimeTest.latency_ms}ms
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
        <Button variant="outline" onClick={handleCancel} disabled={saving || savingConcurrency}>
          {t('config.cancel')}
        </Button>
        <Button onClick={handleSave} disabled={saving || savingConcurrency || !valid}>
          <Save className="h-5 w-5 mr-2" />
          {saving ? t('config.saving') : t('config.save')}
        </Button>
      </div>
    </div>
  );
}
