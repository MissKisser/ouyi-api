import { useState } from 'react';
import useSWR from 'swr';
import {
  apiFetch,
  apiFetchRuntimeLogs,
  apiFetchToolRuntimeLogs,
  type RequestLog,
  type RuntimeLogsResponse,
  type ToolRuntimeLog,
} from '@/lib/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { ScrollText, Terminal, Wrench, ChevronLeft, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';

const PAGE_SIZE = 50;

function formatTime(ts: number, locale: string): string {
  return new Date(ts * 1000).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US');
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

function formatLatency(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
  return `${ms}ms`;
}

const levelVariant = (level: string) => {
  switch (level) {
    case 'ERROR': return 'destructive';
    case 'WARN': return 'secondary';
    case 'INFO': return 'outline';
    case 'DEBUG': return 'outline';
    case 'TRACE': return 'outline';
    default: return 'outline';
  }
};

const levelColor = (level: string) => {
  switch (level) {
    case 'ERROR': return 'bg-red-500/15 text-red-700 border-red-200';
    case 'WARN': return 'bg-yellow-500/15 text-yellow-700 border-yellow-200';
    case 'INFO': return 'bg-blue-500/15 text-blue-700 border-blue-200';
    case 'DEBUG': return 'bg-gray-500/15 text-gray-600 border-gray-200';
    case 'TRACE': return 'bg-gray-500/10 text-gray-500 border-gray-200';
    default: return '';
  }
};

// ── 请求日志 Tab ──────────────────────────────────────────────────────────

function RequestLogsTab() {
  const { t, i18n } = useTranslation();
  const { data: logs, error } = useSWR<RequestLog[]>(
    '/admin/api/logs?limit=100',
    (url: string) => apiFetch<RequestLog[]>(url),
    { refreshInterval: 5000 },
  );

  if (error) {
    return (
      <Card>
        <CardContent className="p-4 text-destructive">
          {t('common.loadError')}
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm text-muted-foreground">
          {t('logs.requestLogs.description')}
        </CardTitle>
      </CardHeader>
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('logs.requestLogs.time')}</TableHead>
              <TableHead>{t('logs.requestLogs.model')}</TableHead>
              <TableHead>{t('logs.requestLogs.apiKey')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.promptTokens')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.completionTokens')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.latency')}</TableHead>
              <TableHead>{t('logs.requestLogs.status')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {logs?.map((log, i) => (
              <TableRow key={i}>
                <TableCell className="text-xs text-muted-foreground whitespace-nowrap">
                  {formatTime(log.timestamp, i18n.language)}
                </TableCell>
                <TableCell className="font-mono text-xs">{log.model}</TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">{log.api_key}</TableCell>
                <TableCell className="text-right text-xs text-blue-600">{formatTokens(log.prompt_tokens)}</TableCell>
                <TableCell className="text-right text-xs text-emerald-600">{formatTokens(log.completion_tokens)}</TableCell>
                <TableCell className="text-right text-xs">{formatLatency(log.latency_ms)}</TableCell>
                <TableCell>
                  {log.success ? (
                    <Badge variant="outline" className="bg-green-500/15 text-green-700 border-green-200 text-xs">
                      {t('logs.requestLogs.success')}
                    </Badge>
                  ) : (
                    <Badge variant="outline" className="bg-red-500/15 text-red-700 border-red-200 text-xs">
                      {t('logs.requestLogs.failure')}
                    </Badge>
                  )}
                </TableCell>
              </TableRow>
            ))}
            {!logs && (
              <TableRow>
                <TableCell colSpan={7} className="text-center text-muted-foreground py-8">
                  {t('logs.requestLogs.loading')}
                </TableCell>
              </TableRow>
            )}
            {logs && logs.length === 0 && (
              <TableRow>
                <TableCell colSpan={7} className="text-center text-muted-foreground py-8">
                  {t('logs.requestLogs.empty')}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

// ── 运行日志 Tab ──────────────────────────────────────────────────────────

function RuntimeLogsTab() {
  const { t } = useTranslation();
  const [page, setPage] = useState(0);
  const offset = page * PAGE_SIZE;

  const { data, isLoading, error } = useSWR<RuntimeLogsResponse>(
    `/admin/api/runtime-logs?offset=${offset}&limit=${PAGE_SIZE}`,
    () => apiFetchRuntimeLogs(offset, PAGE_SIZE),
    { refreshInterval: 3000 },
  );

  if (error) {
    return (
      <Card>
        <CardContent className="p-4 text-destructive">
          {t('common.loadError')}
        </CardContent>
      </Card>
    );
  }

  const totalPages = data ? Math.ceil(data.total / PAGE_SIZE) : 0;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm text-muted-foreground">
          {t('logs.runtimeLogs.description', { total: data?.total ?? '-' })}
        </CardTitle>
      </CardHeader>
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="w-44">{t('logs.runtimeLogs.time')}</TableHead>
              <TableHead className="w-20">{t('logs.runtimeLogs.level')}</TableHead>
              <TableHead className="w-40">{t('logs.runtimeLogs.module')}</TableHead>
              <TableHead>{t('logs.runtimeLogs.message')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data?.logs.map((log, i) => (
              <TableRow key={i}>
                <TableCell className="text-xs text-muted-foreground whitespace-nowrap font-mono">
                  {log.timestamp}
                </TableCell>
                <TableCell>
                  <Badge variant={levelVariant(log.level)} className={`text-xs ${levelColor(log.level)}`}>
                    {log.level}
                  </Badge>
                </TableCell>
                <TableCell className="text-xs font-mono text-muted-foreground">
                  {log.target}
                </TableCell>
                <TableCell className="text-xs break-all">
                  {log.message}
                </TableCell>
              </TableRow>
            ))}
            {isLoading && (
              <TableRow>
                <TableCell colSpan={4} className="text-center text-muted-foreground py-8">
                  {t('logs.runtimeLogs.loading')}
                </TableCell>
              </TableRow>
            )}
            {data && data.logs.length === 0 && (
              <TableRow>
                <TableCell colSpan={4} className="text-center text-muted-foreground py-8">
                  {t('logs.runtimeLogs.empty')}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>

        {/* Pagination */}
        {totalPages > 1 && (
          <div className="flex items-center justify-between px-4 py-3 border-t">
            <span className="text-sm text-muted-foreground">
              {t('logs.runtimeLogs.pageInfo', { current: page + 1, total: totalPages })}
            </span>
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage(p => Math.max(0, p - 1))}
                disabled={page === 0}
              >
                <ChevronLeft className="h-4 w-4 mr-1" />
                {t('logs.runtimeLogs.prev')}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage(p => Math.min(totalPages - 1, p + 1))}
                disabled={page >= totalPages - 1}
              >
                {t('logs.runtimeLogs.next')}
                <ChevronRight className="h-4 w-4 ml-1" />
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ── 工具日志 Tab ──────────────────────────────────────────────────────────

function ToolRuntimeLogsTab() {
  const { t } = useTranslation();

  const { data: logs, isLoading, error } = useSWR<ToolRuntimeLog[]>(
    '/admin/api/tool-runtime-logs?limit=100',
    () => apiFetchToolRuntimeLogs(100),
    { refreshInterval: 5000 },
  );

  if (error) {
    return (
      <Card>
        <CardContent className="p-4 text-destructive">
          {t('common.loadError')}
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm text-muted-foreground">
          {t('logs.toolRuntimeLogs.description', { defaultValue: 'Tool model logs (auto-refresh every 5s)' })}
        </CardTitle>
      </CardHeader>
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t('logs.requestLogs.time')}</TableHead>
              <TableHead>{t('logs.toolRuntimeLogs.endpoint', { defaultValue: 'Endpoint' })}</TableHead>
              <TableHead>{t('logs.toolRuntimeLogs.provider', { defaultValue: 'Provider' })}</TableHead>
              <TableHead>{t('logs.requestLogs.model')}</TableHead>
              <TableHead>{t('logs.toolRuntimeLogs.decision')}</TableHead>
              <TableHead>{t('logs.toolRuntimeLogs.toolNames')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.promptTokens')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.completionTokens')}</TableHead>
              <TableHead className="text-right">{t('logs.requestLogs.latency')}</TableHead>
              <TableHead>{t('logs.requestLogs.status')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {logs?.map((log, i) => (
              <TableRow key={i}>
                <TableCell className="text-xs text-muted-foreground whitespace-nowrap">
                  {log.timestamp}
                </TableCell>
                <TableCell>
                  <Badge
                    variant="outline"
                    className={`text-xs ${
                      log.endpoint === 'openai'
                        ? 'bg-purple-500/15 text-purple-700 border-purple-200'
                        : 'bg-orange-500/15 text-orange-700 border-orange-200'
                    }`}
                  >
                    {log.endpoint}
                  </Badge>
                </TableCell>
                <TableCell className="text-xs font-mono">{log.provider}</TableCell>
                <TableCell className="text-xs font-mono">{log.model}</TableCell>
                <TableCell>
                  <Badge
                    variant="outline"
                    className={`text-xs ${
                      log.decision === 'tool_calls'
                        ? 'bg-blue-500/15 text-blue-700 border-blue-200'
                        : 'bg-emerald-500/15 text-emerald-700 border-emerald-200'
                    }`}
                  >
                    {log.decision}
                  </Badge>
                </TableCell>
                <TableCell className="text-xs max-w-48 break-all">
                  {log.tool_names.length > 0 ? log.tool_names.join(', ') : '-'}
                </TableCell>
                <TableCell className="text-right text-xs text-blue-600">{formatTokens(log.prompt_tokens)}</TableCell>
                <TableCell className="text-right text-xs text-emerald-600">{formatTokens(log.completion_tokens)}</TableCell>
                <TableCell className="text-right text-xs">{formatLatency(log.latency_ms)}</TableCell>
                <TableCell>
                  {log.success ? (
                    <Badge variant="outline" className="bg-green-500/15 text-green-700 border-green-200 text-xs">
                      {t('logs.requestLogs.success')}
                    </Badge>
                  ) : (
                    <Badge variant="outline" className="bg-red-500/15 text-red-700 border-red-200 text-xs">
                      {t('logs.requestLogs.failure')}
                    </Badge>
                  )}
                </TableCell>
              </TableRow>
            ))}
            {isLoading && (
              <TableRow>
                <TableCell colSpan={10} className="text-center text-muted-foreground py-8">
                  {t('logs.requestLogs.loading')}
                </TableCell>
              </TableRow>
            )}
            {logs && logs.length === 0 && (
              <TableRow>
                <TableCell colSpan={10} className="text-center text-muted-foreground py-8">
                  {t('logs.toolRuntimeLogs.empty', { defaultValue: 'No tool runtime logs' })}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

// ── 主页面 ────────────────────────────────────────────────────────────────

export function LogsPage() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<'request' | 'runtime' | 'toolRuntime'>('request');

  return (
    <div className="space-y-6">
      {/* 标题 + Tab 切换 —— 钉在主区域顶部，不随日志表格滚动 */}
      <div className="sticky top-0 z-10 -mx-6 px-6 pt-3 pb-3 bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80 border-b flex flex-col gap-3">
        <h1 className="text-2xl font-bold flex items-center gap-2">
          <ScrollText className="h-6 w-6" />
          {t('logs.title')}
        </h1>

        {/* Tab switcher */}
        <div className="flex gap-2">
          <Button
            variant={tab === 'request' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTab('request')}
          >
            <ScrollText className="h-4 w-4 mr-1" />
            {t('logs.tabs.request')}
          </Button>
          <Button
            variant={tab === 'runtime' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTab('runtime')}
          >
            <Terminal className="h-4 w-4 mr-1" />
            {t('logs.tabs.runtime')}
          </Button>
          <Button
            variant={tab === 'toolRuntime' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTab('toolRuntime')}
          >
            <Wrench className="h-4 w-4 mr-1" />
            {t('logs.tabs.toolRuntime')}
          </Button>
        </div>
      </div>

      {tab === 'request' && <RequestLogsTab />}
      {tab === 'runtime' && <RuntimeLogsTab />}
      {tab === 'toolRuntime' && <ToolRuntimeLogsTab />}
    </div>
  );
}
