import { useState, useCallback, useMemo } from 'react';
import useSWR from 'swr';
import {
  apiFetch,
  apiFetchCategorizedModels,
  type CategorizedModelGroup,
  type ModelListResponse,
  type CategorizedModelsResponse,
} from '@/lib/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Input } from '@/components/ui/input';
import { Boxes, Copy, Check, Search, ChevronDown, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { compareProviders, localizeProviderName } from '@/lib/model-labels';

/**
 * 按激活的供应商 chip 与自由文本搜索过滤分类模型分组。
 * 查询不区分大小写，同时匹配分组的供应商名称/key 与每个模型的 label/value，
 * 因此搜索 "openai" 或 "gpt-4o-mini" 都能定位到正确的分组；模型数为 0 的分组被剔除。
 */
function filterCategorizedGroups(
  groups: CategorizedModelGroup[],
  activeProvider: string | null,
  search: string,
): CategorizedModelGroup[] {
  const query = search.toLowerCase().trim();
  return groups
    .filter(g => !activeProvider || g.provider_key === activeProvider)
    .map(g => {
      const groupMatches = query
        ? g.provider_name.toLowerCase().includes(query) || g.provider_key.toLowerCase().includes(query)
        : true;
      if (!query) return g;
      return {
        ...g,
        models: groupMatches
          ? g.models
          : g.models.filter(
              m => m.label.toLowerCase().includes(query) || m.value.toLowerCase().includes(query),
            ),
      };
    })
    .filter(g => g.models.length > 0);
}

/**
 * 模型列表页：优先按供应商分组展示模型（SWR 请求分类接口），支持搜索、
 * 供应商 chip 筛选与分组折叠；分类接口不可用时回退为扁平卡片网格。
 * 每个模型支持一键复制模型 ID（复制后短暂显示已复制状态）。
 * 副作用：发起模型列表请求、写入剪贴板。
 */
export function ModelsPage() {
  const { t, i18n } = useTranslation();
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [activeProvider, setActiveProvider] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const handleCopy = useCallback((id: string) => {
    navigator.clipboard.writeText(id);
    setCopiedId(id);
    setTimeout(() => setCopiedId(null), 1500);
  }, []);

  const toggleCollapse = useCallback((key: string) => {
    setCollapsed(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const { data: categorizedResponse, error, isLoading } = useSWR<CategorizedModelsResponse>(
    '/admin/api/models/categorized',
    () => apiFetchCategorizedModels(),
  );

  const categorizedGroups = useMemo(
    () => (categorizedResponse?.categories ?? []).slice().sort((a, b) => compareProviders(a.provider_name, b.provider_name)),
    [categorizedResponse?.categories],
  );

  // 仅在分类接口不可用时（尚无响应或请求出错）回退到扁平的 /admin/api/models；
  // 分类响应为空分组时仍保持分类视图（保留搜索框与筛选 chips）。
  const hasCategories = categorizedGroups.length > 0;
  const shouldFetchFlat = !hasCategories && !error;
  const { data: models } = useSWR<ModelListResponse>(
    shouldFetchFlat ? '/admin/api/models' : null,
    (url: string) => apiFetch<ModelListResponse>(url),
  );

  const filteredGroups = useMemo(
    () => filterCategorizedGroups(categorizedGroups, activeProvider, search),
    [categorizedGroups, activeProvider, search],
  );

  const filteredFlatModels = useMemo(() => {
    if (hasCategories || !models?.data) return [];
    const q = search.toLowerCase().trim();
    if (!q) return models.data;
    return models.data.filter(m => m.id.toLowerCase().includes(q) || m.owned_by.toLowerCase().includes(q));
  }, [models, search, hasCategories]);

  const totalCount = hasCategories
    ? categorizedGroups.reduce((s, g) => s + g.count, 0)
    : (models?.data.length ?? 0);

  const shownCount = hasCategories
    ? filteredGroups.reduce((s, g) => s + g.models.length, 0)
    : filteredFlatModels.length;

  if (isLoading) {
    return <div className="p-4 text-muted-foreground">{t('models.loading')}</div>;
  }

  if (error) {
    return <div className="p-4 text-destructive">{t('common.loadError')}</div>;
  }

  return (
    <div className="space-y-4">
      {/* 标题 + 搜索 + 分类筛选 —— 整体钉在主区域顶部，不随卡片列表滚动 */}
      <div className="sticky top-0 z-10 -mx-6 px-6 pt-3 pb-3 bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80 border-b flex flex-col gap-3">
        <h1 className="text-2xl font-bold flex items-center gap-2">
          <Boxes className="h-6 w-6" />
          {t('models.title')}
          <Badge variant="secondary" className="text-xs ml-2">
            {shownCount}/{totalCount}
          </Badge>
        </h1>
        <div className="relative w-full">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder={t('models.searchPlaceholder')}
            value={search}
            onChange={e => setSearch(e.target.value)}
            className="pl-8 w-full"
          />
        </div>
        {hasCategories && (
          <div className="flex gap-1.5 overflow-x-auto pb-1 -mx-1 px-1">
            <button
              type="button"
              className={`shrink-0 inline-flex items-center text-xs px-2.5 h-7 rounded-full border transition-colors ${activeProvider === null ? 'bg-primary text-primary-foreground border-primary' : 'bg-background hover:bg-accent hover:text-accent-foreground text-muted-foreground'}`}
              onClick={() => setActiveProvider(null)}
            >
              {t('models.allCategories')}
            </button>
            {categorizedGroups.map(g => {
              const active = activeProvider === g.provider_key;
              return (
                <button
                  key={g.provider_key}
                  type="button"
                  className={`shrink-0 inline-flex items-center text-xs px-2.5 h-7 rounded-full border transition-colors ${active ? 'bg-primary text-primary-foreground border-primary' : 'bg-background hover:bg-accent hover:text-accent-foreground text-muted-foreground'}`}
                  onClick={() => setActiveProvider(active ? null : g.provider_key)}
                >
                  {localizeProviderName(g.provider_name, i18n.language)}
                  <span className={`ml-1 ${active ? 'opacity-80' : 'opacity-60'}`}>{g.count}</span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {hasCategories ? (
        <div className="space-y-3">
          {filteredGroups.map((group) => {
            const isCollapsed = collapsed.has(group.provider_key);
            return (
              <Card key={group.provider_key}>
                <CardHeader
                  className="pb-2 cursor-pointer select-none"
                  onClick={() => toggleCollapse(group.provider_key)}
                >
                  <CardTitle className="text-base flex items-center justify-between">
                    <span className="flex items-center gap-1.5">
                      {isCollapsed
                        ? <ChevronRight className="h-4 w-4 text-muted-foreground" />
                        : <ChevronDown className="h-4 w-4 text-muted-foreground" />}
                      {localizeProviderName(group.provider_name, i18n.language)}
                    </span>
                    <Badge variant="secondary" className="text-xs">
                      {group.models.length} {t('models.available')}
                    </Badge>
                  </CardTitle>
                </CardHeader>
                {!isCollapsed && (
                  <CardContent className="pt-0">
                    <table className="w-full text-sm">
                      <thead>
                        <tr className="text-xs text-muted-foreground border-b">
                          <th className="text-left font-medium py-2 pl-1 pr-3 w-2/5">{t('models.columnName')}</th>
                          <th className="text-left font-medium py-2 px-3">{t('models.columnId')}</th>
                          <th className="text-right font-medium py-2 pl-3 pr-1 w-24">{t('models.columnAction')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {group.models.map((model) => {
                          const isCopied = copiedId === model.value;
                          const sameName = model.label === model.value;
                          return (
                            <tr
                              key={model.value}
                              className="border-b last:border-b-0 hover:bg-muted/40 transition-colors"
                            >
                              <td className="py-2 pl-1 pr-3 align-top">
                                <span className="font-medium">{model.label}</span>
                              </td>
                              <td className="py-2 px-3 align-top">
                                {sameName ? (
                                  <span className="text-muted-foreground text-xs">—</span>
                                ) : (
                                  <code className="text-xs font-mono text-muted-foreground break-all">{model.value}</code>
                                )}
                              </td>
                              <td className="py-2 pl-3 pr-1 align-top text-right">
                                <button
                                  type="button"
                                  onClick={() => handleCopy(model.value)}
                                  className={`inline-flex items-center gap-1 text-xs px-2 h-7 rounded-md border transition-colors ${isCopied ? 'bg-primary text-primary-foreground border-primary' : 'bg-background hover:bg-accent hover:text-accent-foreground text-muted-foreground'}`}
                                  title={model.value}
                                >
                                  {isCopied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
                                  <span>{isCopied ? t('models.copied') : t('models.copy')}</span>
                                </button>
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </CardContent>
                )}
              </Card>
            );
          })}
          {filteredGroups.length === 0 && (
            <div className="text-center text-muted-foreground py-8">
              {t('models.empty')}
            </div>
          )}
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {filteredFlatModels.map((model) => {
            const isCopied = copiedId === model.id;
            return (
            <Card
              key={model.id}
              className="cursor-pointer hover:border-primary/50 transition-colors"
              onClick={() => handleCopy(model.id)}
              title={`${t('models.clickToCopy')}: ${model.id}`}
            >
              <CardHeader className="pb-2">
                <CardTitle className="text-base flex items-center justify-between">
                  <span className="flex items-center gap-1">
                    {isCopied ? <Check className="h-4 w-4 text-green-500" /> : <Copy className="h-3 w-3 opacity-30" />}
                    {model.id}
                  </span>
                  <Badge variant="secondary" className="text-xs">
                    {model.owned_by}
                  </Badge>
                </CardTitle>
              </CardHeader>
              <CardContent>
                <div className="text-sm text-muted-foreground space-y-1">
                  <div>{t('models.type')}: {model.object}</div>
                </div>
              </CardContent>
            </Card>
            );
          })}
          {filteredFlatModels.length === 0 && models && (
            <div className="col-span-full text-center text-muted-foreground py-8">
              {t('models.empty')}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
