import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link2, PlugZap, Copy, Check, Loader2 } from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Separator } from '@/components/ui/separator';

type EndpointStatus =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'ok'; status: number; latencyMs: number }
  | { kind: 'fail'; reason: 'network' | 'timeout' | 'http'; status?: number; latencyMs?: number };

interface EndpointDef {
  key: string;
  path: string;
}

const TEST_TIMEOUT_MS = 5000;

export function ApiEndpointsPage() {
  const { t } = useTranslation();
  const [origin, setOrigin] = useState<string>(() =>
    typeof window !== 'undefined' ? window.location.origin : '',
  );
  const [statuses, setStatuses] = useState<Record<string, EndpointStatus>>({});
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const abortRef = useRef<Record<string, AbortController>>({});

  // 路由切换/前进后退时 origin 会变
  useEffect(() => {
    const sync = () => setOrigin(window.location.origin);
    window.addEventListener('popstate', sync);
    return () => window.removeEventListener('popstate', sync);
  }, []);

  // 卸载时清掉所有未完成的请求
  useEffect(() => {
    const map = abortRef.current;
    return () => {
      for (const ctrl of Object.values(map)) ctrl.abort();
      for (const k of Object.keys(map)) delete map[k];
    };
  }, []);

  // 6 个端点与 router.rs:15-28 一一对应
  const endpoints: EndpointDef[] = useMemo(
    () => [
      { key: 'chat1', path: '/v1/chat/completions' },
      { key: 'chat2', path: '/openai/v1/chat/completions' },
      { key: 'resp1', path: '/v1/responses' },
      { key: 'resp2', path: '/openai/v1/responses' },
      { key: 'anth1', path: '/anthropic/v1/messages' },
      { key: 'anth2', path: '/anthropic/messages' },
    ],
    [],
  );

  const fullUrl = useCallback((path: string) => `${origin}${path}`, [origin]);

  const setStatus = useCallback((key: string, next: EndpointStatus) => {
    setStatuses((prev) => ({ ...prev, [key]: next }));
  }, []);

  const runTest = useCallback(
    async (key: string, path: string) => {
      // 取消同一端点上一次未完成的请求
      abortRef.current[key]?.abort();
      const controller = new AbortController();
      abortRef.current[key] = controller;

      setStatus(key, { kind: 'loading' });
      const timeoutId = window.setTimeout(() => controller.abort(), TEST_TIMEOUT_MS);
      const started = performance.now();

      try {
        const res = await fetch(fullUrl(path), {
          method: 'GET',
          mode: 'cors',
          signal: controller.signal,
        });
        const latencyMs = Math.round(performance.now() - started);
        // 任意 < 500 的响应都算"可联通到网关"
        if (res.status < 500) {
          setStatus(key, { kind: 'ok', status: res.status, latencyMs });
        } else {
          setStatus(key, { kind: 'fail', reason: 'http', status: res.status });
        }
      } catch (e: unknown) {
        const latencyMs = Math.round(performance.now() - started);
        const isAbort = e instanceof DOMException && e.name === 'AbortError';
        setStatus(key, {
          kind: 'fail',
          reason: isAbort ? 'timeout' : 'network',
          latencyMs,
        });
      } finally {
        window.clearTimeout(timeoutId);
        if (abortRef.current[key] === controller) delete abortRef.current[key];
      }
    },
    [fullUrl, setStatus],
  );

  const runAll = useCallback(async () => {
    await Promise.allSettled(endpoints.map((ep) => runTest(ep.key, ep.path)));
  }, [endpoints, runTest]);

  const handleCopy = useCallback((key: string, url: string) => {
    void navigator.clipboard.writeText(url);
    setCopiedKey(key);
    window.setTimeout(() => {
      setCopiedKey((prev) => (prev === key ? null : prev));
    }, 1500);
  }, []);

  const statusBadge = (s: EndpointStatus | undefined) => {
    const status: EndpointStatus = s ?? { kind: 'idle' };
    switch (status.kind) {
      case 'idle':
        return (
          <Badge variant="outline" className="font-normal text-muted-foreground">
            {t('endpoints.statusPending')}
          </Badge>
        );
      case 'loading':
        return (
          <Badge variant="outline" className="font-normal">
            <Loader2 className="h-3 w-3 mr-1 animate-spin" />
            {t('endpoints.testing')}
          </Badge>
        );
      case 'ok':
        return (
          <Badge variant="secondary" className="font-normal">
            ✅ HTTP {status.status} · {status.latencyMs}ms
          </Badge>
        );
      case 'fail': {
        const label =
          status.reason === 'timeout'
            ? t('endpoints.timeout')
            : status.reason === 'http'
              ? `HTTP ${status.status ?? '?'}`
              : t('endpoints.networkError');
        return (
          <Badge variant="destructive" className="font-normal">
            ❌ {label}
          </Badge>
        );
      }
    }
  };

  return (
    <div className="space-y-4">
      {/* 顶部 sticky 标题栏 —— 复用 ModelsPage:122-164 风格 */}
      <div className="sticky top-0 z-10 -mx-6 px-6 pt-3 pb-3 bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80 border-b flex flex-col gap-3">
        <div className="flex items-center justify-between gap-4">
          <h1 className="text-2xl font-bold flex items-center gap-2 shrink-0">
            <Link2 className="h-6 w-6" />
            {t('endpoints.title')}
            <Badge variant="secondary" className="text-xs ml-2">
              {endpoints.length}
            </Badge>
          </h1>
          <Button onClick={runAll} className="shrink-0">
            <PlugZap className="h-4 w-4 mr-2" />
            {t('endpoints.testAll')}
          </Button>
        </div>
        <div className="text-xs text-muted-foreground">
          {t('endpoints.baseLabel')}：
          <code className="ml-1 px-1.5 py-0.5 rounded bg-muted font-mono text-foreground">
            {origin || '(empty)'}
          </code>
        </div>
      </div>

      {/* 卡片列表 —— 双栏网格 */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
        {endpoints.map((ep) => {
          const url = fullUrl(ep.path);
          const status = statuses[ep.key];
          const isLoading = status?.kind === 'loading';
          const isCopied = copiedKey === ep.key;
          return (
            <Card key={ep.key}>
              <CardHeader className="pb-2">
                <CardTitle className="text-base flex items-center justify-between gap-2">
                  <span className="flex items-center gap-2 min-w-0">
                    {t(`endpoints.${ep.key}`)}
                    <Badge variant="outline" className="font-normal text-xs">
                      POST · application/json
                    </Badge>
                  </span>
                  <span className="flex items-center gap-1 shrink-0">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => runTest(ep.key, ep.path)}
                      disabled={isLoading}
                      aria-label={t('endpoints.test')}
                    >
                      {isLoading ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : (
                        <PlugZap className="h-4 w-4" />
                      )}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleCopy(ep.key, url)}
                      aria-label={t('endpoints.copy')}
                    >
                      {isCopied ? (
                        <Check className="h-4 w-4 text-green-600" />
                      ) : (
                        <Copy className="h-4 w-4" />
                      )}
                    </Button>
                  </span>
                </CardTitle>
              </CardHeader>
              <CardContent className="pt-0 space-y-2">
                <code className="block px-3 py-2 rounded bg-muted font-mono text-sm break-all">
                  {url}
                </code>
                <Separator />
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>{t('endpoints.pathLabel')}：{ep.path}</span>
                  {statusBadge(status)}
                </div>
              </CardContent>
            </Card>
          );
        })}
      </div>
    </div>
  );
}