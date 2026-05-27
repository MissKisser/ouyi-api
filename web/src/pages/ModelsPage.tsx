import { useState, useCallback, useMemo } from 'react';
import useSWR from 'swr';
import { apiFetch, apiFetchCategorizedModels, type ModelListResponse, type CategorizedModelsResponse } from '@/lib/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Input } from '@/components/ui/input';
import { Boxes, Copy, Check, Search, ChevronDown, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export function ModelsPage() {
  const { t } = useTranslation();
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
    () => categorizedResponse?.categories ?? [],
    [categorizedResponse?.categories],
  );

  const shouldFetchFlat = categorizedGroups.length === 0;
  const { data: models } = useSWR<ModelListResponse>(
    shouldFetchFlat ? '/admin/api/models' : null,
    (url: string) => apiFetch<ModelListResponse>(url),
  );

  const hasCategories = categorizedGroups.length > 0;

  const filteredGroups = useMemo(() => {
    if (!hasCategories) return [];
    const q = search.toLowerCase().trim();
    return categorizedGroups
      .filter(g => !activeProvider || g.provider_key === activeProvider)
      .map(g => ({
        ...g,
        models: q
          ? g.models.filter(m => m.label.toLowerCase().includes(q) || m.value.toLowerCase().includes(q))
          : g.models,
      }))
      .filter(g => g.models.length > 0);
  }, [categorizedGroups, activeProvider, search, hasCategories]);

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
      <h1 className="text-2xl font-bold flex items-center gap-2">
        <Boxes className="h-6 w-6" />
        {t('models.title')}
        <Badge variant="secondary" className="text-xs ml-2">
          {shownCount}/{totalCount}
        </Badge>
      </h1>

      {/* 搜索 & 分类筛选 */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="relative flex-1 max-w-sm">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder={t('models.searchPlaceholder')}
            value={search}
            onChange={e => setSearch(e.target.value)}
            className="pl-8"
          />
        </div>
        {hasCategories && (
          <div className="flex flex-wrap gap-1.5">
            <Badge
              variant={activeProvider === null ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setActiveProvider(null)}
            >
              {t('models.allCategories')}
            </Badge>
            {categorizedGroups.map(g => (
              <Badge
                key={g.provider_key}
                variant={activeProvider === g.provider_key ? 'default' : 'outline'}
                className="cursor-pointer text-xs"
                onClick={() => setActiveProvider(activeProvider === g.provider_key ? null : g.provider_key)}
              >
                {g.provider_name}
                <span className="ml-1 opacity-60">{g.count}</span>
              </Badge>
            ))}
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
                      {group.provider_name}
                    </span>
                    <Badge variant="secondary" className="text-xs">
                      {group.models.length} {t('models.available')}
                    </Badge>
                  </CardTitle>
                </CardHeader>
                {!isCollapsed && (
                  <CardContent>
                    <div className="flex flex-wrap gap-2">
                      {group.models.map((model) => {
                        const isCopied = copiedId === model.value;
                        return (
                          <Badge
                            key={model.value}
                            variant="outline"
                            className="text-xs cursor-pointer hover:bg-accent hover:text-accent-foreground transition-colors"
                            onClick={() => handleCopy(model.value)}
                            title={`${t('models.clickToCopy')}: ${model.value}`}
                          >
                            {isCopied ? <Check className="h-3 w-3 mr-1" /> : <Copy className="h-3 w-3 mr-1 opacity-50" />}
                            {model.label}
                          </Badge>
                        );
                      })}
                    </div>
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
