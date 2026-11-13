import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Send, Settings2, Trash2, Copy, RefreshCw, Search, Check, ChevronDown } from 'lucide-react';
import { apiFetchModels, apiFetchCategorizedModels, apiFetchModelsOpenAI, type ModelInfo, type CategorizedModelGroup } from '@/lib/api';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { Slider } from '@/components/ui/slider';
import { Input } from '@/components/ui/input';
import { Collapsible, CollapsibleContent } from '@/components/ui/collapsible';
import { compareProviders, localizeProviderName } from '@/lib/model-labels';

/** 聊天界面中的单条消息。 */
interface Message {
  /** 消息角色：用户 / 助手 / 系统。 */
  role: 'user' | 'assistant' | 'system';
  /** 消息文本内容。 */
  content: string;
  /** 消息创建时间，毫秒时间戳（Date.now()）。 */
  timestamp: number;
}

/** 对话采样参数集合（对应 OpenAI 风格请求体字段）。 */
interface ChatParams {
  /** 温度系数，0–2，越高越随机。 */
  temperature: number;
  /** 单次回复的最大输出 token 数。 */
  maxTokens: number;
  /** 核采样累积概率阈值，0–1。 */
  topP: number;
  /** 存在惩罚系数，-2 到 2。 */
  presencePenalty: number;
  /** 频率惩罚系数，-2 到 2。 */
  frequencyPenalty: number;
  /** 是否开启思考模式（携带 thinking 扩展字段）。 */
  thinking: boolean;
  /** 思考模式的 token 预算，单位 token。 */
  thinkingBudget: number;
}

/** 参数面板的默认值，重置按钮恢复到该组数值。 */
const DEFAULT_PARAMS: ChatParams = {
  temperature: 1.0,
  maxTokens: 8192,
  topP: 1.0,
  presencePenalty: 0,
  frequencyPenalty: 0,
  thinking: false,
  thinkingBudget: 4096,
};

/**
 * 从错误响应体中提取可读的错误信息。
 *
 * * `raw` - 响应体原文，通常为 JSON 错误结构。
 * 返回值：解析出的错误消息；无法解析为 JSON 时返回原文，空响应返回兜底文案。
 */
function extractErrorMessage(raw: string): string {
  const text = raw.trim();
  if (!text) return 'Unknown error';
  try {
    const parsed = JSON.parse(text);
    const detail = parsed?.error;
    if (typeof detail === 'string') return detail;
    if (detail?.message) return detail.message;
    if (parsed?.message) return parsed.message;
    return text;
  } catch {
    return text;
  }
}

/**
 * 对话测试页：选择模型后向 /v1/chat/completions 发起流式（SSE）对话，
 * 实时渲染增量回复。模型列表优先取分类接口（按供应商分组 + chip 筛选 + 搜索），
 * 失败时依次回退到 OpenAI 风格与管理端列表接口；支持采样参数面板、
 * 中止生成、清空会话与复制消息。副作用：加载与发送均发起网络请求，
 * 复制写入剪贴板；发送期间以 requestAnimationFrame 节流流式内容更新。
 */
export function ChatTestPage() {
  const { t, i18n } = useTranslation();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [categorizedGroups, setCategorizedGroups] = useState<CategorizedModelGroup[]>([]);
  const [useCategorized, setUseCategorized] = useState(true);
  const [selectedModel, setSelectedModel] = useState('');
  const [modelSearch, setModelSearch] = useState('');
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [providerFilter, setProviderFilter] = useState<string | null>(null);
  const providerChipRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const providerGroupRefs = useRef<Record<string, HTMLDivElement | null>>({});
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [isLoading, setIsLoading] = useState(false);
  const [params, setParams] = useState<ChatParams>(DEFAULT_PARAMS);
  const [paramsOpen, setParamsOpen] = useState(false);
  const [streamContent, setStreamContent] = useState('');
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);
  const messagesRef = useRef(messages);
  const rafRef = useRef<number | null>(null);
  const modelPickerRef = useRef<HTMLDivElement>(null);

  // 当前选中模型的展示名（用于触发器按钮）
  const selectedDisplay = useMemo(() => {
    if (!selectedModel) return '';
    const lang = i18n.language;
    if (useCategorized) {
      for (const group of categorizedGroups) {
        const hit = group.models.find(m => m.value === selectedModel);
        if (hit) return `${hit.label}  ·  ${localizeProviderName(group.provider_name, lang)}`;
      }
    } else {
      const hit = models.find(m => m.id === selectedModel);
      if (hit) return hit.id;
    }
    return selectedModel;
  }, [selectedModel, categorizedGroups, models, useCategorized, i18n.language]);

  // 按 search 过滤后的分组与模型
  const filteredCategorized = useMemo(() => {
    const query = modelSearch.toLowerCase().trim();
    if (!useCategorized) return [];
    return categorizedGroups
      .filter(group => !providerFilter || group.provider_key === providerFilter)
      .map(group => {
        const groupHits = query
          ? group.provider_name.toLowerCase().includes(query) || group.provider_key.toLowerCase().includes(query)
          : true;
        const models = !query
          ? group.models
          : groupHits
            ? group.models
            : group.models.filter(
                m => m.label.toLowerCase().includes(query) || m.value.toLowerCase().includes(query),
              );
        return { ...group, models };
      })
      .filter(group => group.models.length > 0);
  }, [categorizedGroups, modelSearch, useCategorized, providerFilter]);

  const filteredFlat = useMemo(() => {
    const query = modelSearch.toLowerCase().trim();
    if (useCategorized) return [];
    if (!query) return models;
    return models.filter(m => m.id.toLowerCase().includes(query));
  }, [models, modelSearch, useCategorized]);

  // 关闭 picker 的外部点击 / Esc
  useEffect(() => {
    if (!modelPickerOpen) return;
    const onPointer = (e: MouseEvent) => {
      if (modelPickerRef.current && !modelPickerRef.current.contains(e.target as Node)) {
        setModelPickerOpen(false);
        setProviderFilter(null);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setModelPickerOpen(false);
        setProviderFilter(null);
      }
    };
    document.addEventListener('mousedown', onPointer);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onPointer);
      document.removeEventListener('keydown', onKey);
    };
  }, [modelPickerOpen]);

  useEffect(() => {
    messagesRef.current = messages;
  }, [messages]);

  // 当用户点击 chip 切到某个 provider 时，自动滚动 dropdown 到该组
  useEffect(() => {
    if (!modelPickerOpen || !providerFilter) return;
    const id = window.setTimeout(() => {
      const el = providerGroupRefs.current[providerFilter];
      if (el) el.scrollIntoView({ block: 'start', behavior: 'smooth' });
    }, 50);
    return () => window.clearTimeout(id);
  }, [modelPickerOpen, providerFilter, modelSearch]);  // 加载模型列表
  useEffect(() => {
    const fetchModels = async () => {
      try {
        const categorized = await apiFetchCategorizedModels();
        const groups = (categorized.categories ?? [])
          .slice()
          .sort((a, b) => compareProviders(a.provider_name, b.provider_name));
        if (groups.length > 0) {
          setCategorizedGroups(groups);
          setUseCategorized(true);
          const allModels = groups.flatMap(g => g.models);
          if (allModels.length > 0) setSelectedModel(allModels[0].value);
        } else {
          const openaiModels = await apiFetchModelsOpenAI();
          const flat = openaiModels.data.map(m => ({ ...m, display: m.id }));
          setModels(flat);
          setUseCategorized(false);
          if (flat.length > 0) setSelectedModel(flat[0].id);
        }
      } catch {
        try {
          const result = await apiFetchModels();
          const flat = result.data.map(m => ({ ...m, display: m.id }));
          setModels(flat);
          setUseCategorized(false);
          if (flat.length > 0) setSelectedModel(flat[0].id);
        } catch {
          // 模型加载失败，保持空列表
        }
      }
    };
    fetchModels();
  }, []);

  // 自动滚动到底部
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, streamContent]);

  const handleSend = useCallback(async () => {
    if (!input.trim() || !selectedModel || isLoading) return;

    const userMessage: Message = { role: 'user', content: input.trim(), timestamp: Date.now() };
    setMessages(prev => [...prev, userMessage]);
    setInput('');
    setIsLoading(true);
    setStreamContent('');

    const abortController = new AbortController();
    abortRef.current = abortController;

    const currentMessages = messagesRef.current;
    const systemMessages = currentMessages.filter(m => m.role === 'system');
    const apiMessages = [
      ...systemMessages,
      ...currentMessages.filter(m => m.role !== 'system'),
      { role: 'user' as const, content: input.trim() },
    ];

    try {
      const body: Record<string, unknown> = {
        model: selectedModel,
        messages: apiMessages,
        temperature: params.temperature,
        max_tokens: params.maxTokens,
        top_p: params.topP,
        presence_penalty: params.presencePenalty,
        frequency_penalty: params.frequencyPenalty,
        stream: true,
      };

      if (params.thinking) {
        body.thinking = { type: 'enabled', budget_tokens: params.thinkingBudget };
      }

      const response = await fetch('/v1/chat/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: abortController.signal,
      });

      if (!response.ok) {
        const raw = await response.text().catch(() => '');
        throw new Error(`HTTP ${response.status}: ${extractErrorMessage(raw)}`);
      }

      const reader = response.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';
      let fullContent = '';
      let sseEvent = '';
      let sseError = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed) continue;
          if (trimmed.startsWith('event:')) {
            sseEvent = trimmed.slice(6).trim();
            continue;
          }
          if (trimmed === 'data: [DONE]') continue;
          if (!trimmed.startsWith('data: ')) continue;

          try {
            const json = JSON.parse(trimmed.slice(6));
            const payloadError = json.error;
            if (sseEvent === 'error' || payloadError) {
              sseError = payloadError?.message ?? json.message ?? sseError;
              sseEvent = '';
              continue;
            }
            sseEvent = '';
            const delta = json.choices?.[0]?.delta;
            if (delta?.content) {
              fullContent += delta.content;
              if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
              rafRef.current = requestAnimationFrame(() => {
                setStreamContent(fullContent);
              });
            }
          } catch {
            // 跳过格式错误的 SSE 事件
          }
        }
      }

      if (fullContent) {
        setMessages(prev => [...prev, { role: 'assistant', content: fullContent, timestamp: Date.now() }]);
      }

      if (sseError) {
        throw new Error(sseError);
      }
    } catch (e: unknown) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      const errorMsg = e instanceof Error ? e.message : 'Unknown error';
      setMessages(prev => [...prev, { role: 'assistant', content: `Error: ${errorMsg}`, timestamp: Date.now() }]);
    } finally {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      setIsLoading(false);
      setStreamContent('');
      abortRef.current = null;
    }
  }, [input, selectedModel, params, isLoading]);

  const handleStop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const handleClear = useCallback(() => {
    setMessages([]);
    setStreamContent('');
    abortRef.current?.abort();
    setIsLoading(false);
  }, []);

  const handleParamChange = useCallback((key: keyof ChatParams, value: number | boolean) => {
    setParams(prev => ({ ...prev, [key]: value }));
  }, []);

  const handleCopy = useCallback((content: string) => {
    navigator.clipboard.writeText(content);
  }, []);

  return (
    <div className="flex flex-col h-[calc(100vh-4rem)] -m-6">
      {/* 顶部栏：模型选择 + 参数面板 */}
      <div className="flex items-center gap-4 px-4 py-2 border-b bg-card shrink-0">
        <div className="flex-1 min-w-0 relative" ref={modelPickerRef}>
          <Button
            type="button"
            variant="outline"
            className="w-full justify-between font-normal h-9 px-3 min-w-0"
            onClick={() => {
              if (modelPickerOpen) {
                setModelPickerOpen(false);
                setProviderFilter(null);
              } else {
                setModelSearch('');
                setProviderFilter(null);
                setModelPickerOpen(true);
              }
            }}
            disabled={!selectedModel && categorizedGroups.length === 0 && models.length === 0}
          >
            <span className="truncate text-left flex-1">
              {selectedDisplay || t('chat.selectModel')}
            </span>
            <ChevronDown className="h-4 w-4 ml-2 shrink-0 opacity-60" />
          </Button>
          {useCategorized && categorizedGroups.length > 0 && (
            <div className="mt-2 flex gap-1.5 overflow-x-auto pb-1 -mx-1 px-1">
              <button
                type="button"
                className={`shrink-0 inline-flex items-center text-xs px-2.5 h-7 rounded-full border transition-colors ${providerFilter === null ? 'bg-primary text-primary-foreground border-primary' : 'bg-background hover:bg-accent hover:text-accent-foreground text-muted-foreground'}`}
                onClick={() => {
                  setProviderFilter(null);
                  if (!modelPickerOpen) setModelPickerOpen(true);
                }}
              >
                {t('models.allCategories')}
              </button>
              {categorizedGroups.map(group => {
                const active = providerFilter === group.provider_key;
                return (
                  <button
                    key={group.provider_key}
                    type="button"
                    ref={el => {
                      providerChipRefs.current[group.provider_key] = el;
                    }}
                    className={`shrink-0 inline-flex items-center text-xs px-2.5 h-7 rounded-full border transition-colors ${active ? 'bg-primary text-primary-foreground border-primary' : 'bg-background hover:bg-accent hover:text-accent-foreground text-muted-foreground'}`}
                    onClick={() => {
                      setProviderFilter(group.provider_key);
                      if (!modelPickerOpen) setModelPickerOpen(true);
                    }}
                  >
                    {localizeProviderName(group.provider_name, i18n.language)}
                    <span className={`ml-1 ${active ? 'opacity-80' : 'opacity-60'}`}>{group.count}</span>
                  </button>
                );
              })}
            </div>
          )}
          {modelPickerOpen && (
            <div className="absolute z-50 mt-1 w-full max-h-[60vh] overflow-hidden flex flex-col rounded-md border bg-popover shadow-lg">
              <div className="p-2 border-b shrink-0">
                <div className="relative">
                  <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                  <Input
                    autoFocus
                    value={modelSearch}
                    onChange={e => {
                      setModelSearch(e.target.value);
                      if (providerFilter) setProviderFilter(null);
                    }}
                    placeholder={t('chat.searchModel')}
                    className="pl-8 h-8"
                    onKeyDown={e => {
                      if (e.key === 'Enter' && modelSearch.trim()) {
                        // 回车直接选中第一个可见项
                        const first = useCategorized
                          ? filteredCategorized[0]?.models[0]
                          : filteredFlat[0];
                        if (first) {
                          const value = useCategorized
                            ? (first as { value: string }).value
                            : (first as { id: string }).id;
                          setSelectedModel(value);
                          setModelPickerOpen(false);
                          e.preventDefault();
                        }
                      }
                    }}
                  />
                </div>
              </div>
              <div className="overflow-y-auto p-1 text-sm">
                {useCategorized ? (
                  filteredCategorized.length === 0 ? (
                    <div className="px-3 py-6 text-center text-muted-foreground text-xs">
                      {t('chat.noMatches')}
                    </div>
                  ) : (
                    filteredCategorized.map(group => (
                      <div
                        key={group.provider_key}
                        className="mb-1"
                        ref={el => {
                          providerGroupRefs.current[group.provider_key] = el;
                        }}
                      >
                        <div className="px-2 pt-2 pb-1 text-xs font-medium text-muted-foreground sticky top-0 bg-popover">
                          {localizeProviderName(group.provider_name, i18n.language)} <span className="opacity-60">({group.models.length})</span>
                        </div>
                        {group.models.map(m => {
                          const active = m.value === selectedModel;
                          return (
                            <button
                              key={m.value}
                              type="button"
                              className={`w-full text-left px-2 py-1.5 rounded-sm hover:bg-accent hover:text-accent-foreground flex items-center gap-2 ${active ? 'bg-accent/50' : ''}`}
                              onClick={() => {
                                setSelectedModel(m.value);
                                setModelPickerOpen(false);
                              }}
                            >
                              <span className="flex-1 truncate">{m.label}</span>
                              {active && <Check className="h-3.5 w-3.5 text-primary shrink-0" />}
                            </button>
                          );
                        })}
                      </div>
                    ))
                  )
                ) : filteredFlat.length === 0 ? (
                  <div className="px-3 py-6 text-center text-muted-foreground text-xs">
                    {t('chat.noMatches')}
                  </div>
                ) : (
                  filteredFlat.map(m => {
                    const active = m.id === selectedModel;
                    return (
                      <button
                        key={m.id}
                        type="button"
                        className={`w-full text-left px-2 py-1.5 rounded-sm hover:bg-accent hover:text-accent-foreground flex items-center gap-2 ${active ? 'bg-accent/50' : ''}`}
                        onClick={() => {
                          setSelectedModel(m.id);
                          setModelPickerOpen(false);
                        }}
                      >
                        <span className="flex-1 truncate">{m.id}</span>
                        {active && <Check className="h-3.5 w-3.5 text-primary shrink-0" />}
                      </button>
                    );
                  })
                )}
              </div>
            </div>
          )}
        </div>

        <div className="relative">
          <Collapsible open={paramsOpen} onOpenChange={setParamsOpen} className="shrink-0">
            <span onClick={() => setParamsOpen(prev => !prev)}
              className="inline-flex items-center justify-center rounded-md text-sm font-medium border bg-background px-3 py-1.5 h-9 gap-2 hover:bg-accent hover:text-accent-foreground cursor-pointer">
              <Settings2 className="h-4 w-4" />
              {t('chat.params')}
            </span>
            <CollapsibleContent open={paramsOpen} className="absolute right-0 top-full mt-2 w-80 bg-card border rounded-lg shadow-lg p-4 z-50 space-y-3">
              <div className="space-y-2">
                <Label className="flex justify-between">
                  <span>{t('chat.temperature')}</span>
                  <span className="text-muted-foreground">{params.temperature.toFixed(1)}</span>
                </Label>
                <Slider min={0} max={2} step={0.1} value={[params.temperature]}
                  onValueChange={v => handleParamChange('temperature', v[0] as number)} />
              </div>
              <div className="space-y-2">
                <Label className="flex justify-between">
                  <span>{t('chat.topP')}</span>
                  <span className="text-muted-foreground">{params.topP.toFixed(1)}</span>
                </Label>
                <Slider min={0} max={1} step={0.05} value={[params.topP]}
                  onValueChange={v => handleParamChange('topP', v[0] as number)} />
              </div>
              <div className="space-y-2">
                <Label className="flex justify-between">
                  <span>{t('chat.maxTokens')}</span>
                  <span className="text-muted-foreground">{params.maxTokens}</span>
                </Label>
                <Slider min={256} max={32768} step={256} value={[params.maxTokens]}
                  onValueChange={v => handleParamChange('maxTokens', v[0] as number)} />
              </div>
              <div className="space-y-2">
                <Label className="flex justify-between">
                  <span>{t('chat.presencePenalty')}</span>
                  <span className="text-muted-foreground">{params.presencePenalty.toFixed(1)}</span>
                </Label>
                <Slider min={-2} max={2} step={0.1} value={[params.presencePenalty]}
                  onValueChange={v => handleParamChange('presencePenalty', v[0] as number)} />
              </div>
              <div className="space-y-2">
                <Label className="flex justify-between">
                  <span>{t('chat.frequencyPenalty')}</span>
                  <span className="text-muted-foreground">{params.frequencyPenalty.toFixed(1)}</span>
                </Label>
                <Slider min={-2} max={2} step={0.1} value={[params.frequencyPenalty]}
                  onValueChange={v => handleParamChange('frequencyPenalty', v[0] as number)} />
              </div>
              <div className="flex items-center justify-between">
                <Label>{t('chat.thinking')}</Label>
                <Switch checked={params.thinking} onCheckedChange={v => handleParamChange('thinking', v)} />
              </div>
              {params.thinking && (
                <div className="space-y-2">
                  <Label className="flex justify-between">
                    <span>{t('chat.thinkingBudget')}</span>
                    <span className="text-muted-foreground">{params.thinkingBudget}</span>
                  </Label>
                  <Slider min={1024} max={16384} step={1024} value={[params.thinkingBudget]}
                    onValueChange={v => handleParamChange('thinkingBudget', v[0] as number)} />
                </div>
              )}
              <Button variant="outline" size="sm" className="w-full"
                onClick={() => setParams({ ...DEFAULT_PARAMS })}>
                {t('chat.resetParams')}
              </Button>
            </CollapsibleContent>
          </Collapsible>
        </div>

        <Button variant="ghost" size="sm" onClick={handleClear} disabled={messages.length === 0 && !isLoading}>
          <Trash2 className="h-4 w-4" />
        </Button>
        <Button variant="ghost" size="sm" onClick={() => window.location.reload()}>
          <RefreshCw className="h-4 w-4" />
        </Button>
      </div>

      {/* 消息区域 */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && !streamContent && (
          <div className="flex items-center justify-center h-full text-muted-foreground">
            <p>{t('chat.welcome')}</p>
          </div>
        )}
        {messages.map((msg, i) => (
          <div key={i} className={`flex ${msg.role === 'user' ? 'justify-end' : 'justify-start'}`}>
            <div className={`max-w-[70%] rounded-lg px-4 py-2 ${
              msg.role === 'user'
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted'
            }`}>
              <div className="whitespace-pre-wrap break-words text-sm">{msg.content}</div>
              <div className="flex items-center gap-1 mt-1">
                <Button variant="ghost" size="sm" className="h-4 w-4 p-0 text-xs opacity-50 hover:opacity-100"
                  onClick={() => handleCopy(msg.content)}>
                  <Copy className="h-3 w-3" />
                </Button>
              </div>
            </div>
          </div>
        ))}
        {streamContent && (
          <div className="flex justify-start">
            <div className="max-w-[70%] rounded-lg px-4 py-2 bg-muted">
              <div className="whitespace-pre-wrap break-words text-sm">{streamContent}</div>
              <div className="animate-pulse text-xs text-muted-foreground mt-1">
                {t('chat.streaming')}
              </div>
            </div>
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* 输入区域 */}
      <div className="border-t bg-card p-4 shrink-0">
        <div className="flex gap-2 max-w-3xl mx-auto">
          <Textarea
            value={input}
            onChange={e => setInput(e.target.value)}
            placeholder={t('chat.placeholder')}
            className="flex-1 resize-none min-h-[44px] max-h-[200px]"
            onKeyDown={e => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                handleSend();
              }
            }}
            disabled={isLoading}
            rows={1}
          />
          {isLoading ? (
            <Button onClick={handleStop} variant="destructive" className="shrink-0">
              {t('chat.stop')}
            </Button>
          ) : (
            <Button onClick={handleSend} disabled={!input.trim() || !selectedModel} className="shrink-0">
              <Send className="h-4 w-4" />
            </Button>
          )}
        </div>
        <div className="text-xs text-muted-foreground text-center mt-2">
          {t('chat.hint')}
        </div>
      </div>
    </div>
  );
}
