import { useState, useCallback } from 'react';
import useSWR from 'swr';
import { apiFetch, apiFetchCategorizedModels, type ModelListResponse, type CategorizedModelGroup } from '@/lib/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Boxes, Copy, Check } from 'lucide-react';
import { useTranslation } from 'react-i18next';

export function ModelsPage() {
  const { t } = useTranslation();
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const handleCopy = useCallback((id: string) => {
    navigator.clipboard.writeText(id);
    setCopiedId(id);
    setTimeout(() => setCopiedId(null), 1500);
  }, []);

  const { data: categorizedGroups, error, isLoading } = useSWR<CategorizedModelGroup[]>(
    '/admin/api/models/categorized',
    () => apiFetchCategorizedModels(),
  );

  const shouldFetchFlat = categorizedGroups && categorizedGroups.length === 0;
  const { data: models } = useSWR<ModelListResponse>(
    shouldFetchFlat ? '/admin/api/models' : null,
    (url: string) => apiFetch<ModelListResponse>(url),
  );

  const hasCategories = categorizedGroups && categorizedGroups.length > 0;

  if (isLoading) {
    return <div className="p-4 text-muted-foreground">{t('models.loading')}</div>;
  }

  if (error) {
    return <div className="p-4 text-destructive">{t('common.loadError')}</div>;
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold flex items-center gap-2">
        <Boxes className="h-6 w-6" />
        {t('models.title')}
      </h1>

      {hasCategories ? (
        <div className="space-y-6">
          {categorizedGroups.map((group) => (
            <Card key={group.provider_key}>
              <CardHeader className="pb-2">
                <CardTitle className="text-base flex items-center justify-between">
                  <span>{group.provider_name}</span>
                  <Badge variant="secondary" className="text-xs">
                    {group.count} {t('models.available')}
                  </Badge>
                </CardTitle>
              </CardHeader>
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
            </Card>
          ))}
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {models?.data.map((model) => {
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
          {!models && (
            <div className="col-span-full text-center text-muted-foreground py-8">
              {t('models.loading')}
            </div>
          )}
          {models && models.data.length === 0 && (
            <div className="col-span-full text-center text-muted-foreground py-8">
              {t('models.empty')}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
