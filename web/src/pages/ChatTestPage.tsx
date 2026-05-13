import { useState, useRef, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Send, Settings2, Trash2, Copy, RefreshCw } from 'lucide-react';
import { apiFetchModels, apiFetchCategorizedModels, apiFetchModelsOpenAI, type ModelInfo, type CategorizedModelGroup } from '@/lib/api';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { Slider } from '@/components/ui/slider';
import { Select, SelectGroup, SelectItem } from '@/components/ui/select';
import { Collapsible, CollapsibleContent } from '@/components/ui/collapsible';

interface Message {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: number;
}

interface ChatParams {
  temperature: number;
  maxTokens: number;
  topP: number;
  presencePenalty: number;
  frequencyPenalty: number;
  thinking: boolean;
  thinkingBudget: number;
}

const DEFAULT_PARAMS: ChatParams = {
  temperature: 1.0,
  maxTokens: 8192,
  topP: 1.0,
  presencePenalty: 0,
  frequencyPenalty: 0,
  thinking: false,
  thinkingBudget: 4096,
};

export function ChatTestPage() {
  const { t } = useTranslation();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [categorizedGroups, setCategorizedGroups] = useState<CategorizedModelGroup[]>([]);
  const [useCategorized, setUseCategorized] = useState(true);
  const [selectedModel, setSelectedModel] = useState('');
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

  useEffect(() => {
    messagesRef.current = messages;
  }, [messages]);

  // 加载模型列表
  useEffect(() => {
    const fetchModels = async () => {
      try {
        const categorized = await apiFetchCategorizedModels();
        if (categorized.length > 0) {
          setCategorizedGroups(categorized);
          setUseCategorized(true);
          const allModels = categorized.flatMap(g => g.models);
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
        throw new Error(`HTTP ${response.status}`);
      }

      const reader = response.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';
      let fullContent = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || trimmed === 'data: [DONE]') continue;
          if (!trimmed.startsWith('data: ')) continue;

          try {
            const json = JSON.parse(trimmed.slice(6));
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
        <div className="flex-1">
          <Select
            value={selectedModel}
            onValueChange={setSelectedModel}
            placeholder={t('chat.selectModel')}
            className="w-full max-w-xs"
          >
            {useCategorized
              ? categorizedGroups.map(group => (
                  <SelectGroup key={group.provider_key} label={group.provider_name}>
                    {group.models.map(m => (
                      <SelectItem key={m.value} value={m.value}>{m.label}</SelectItem>
                    ))}
                  </SelectGroup>
                ))
              : models.map(m => (
                  <SelectItem key={m.id} value={m.id}>{(m as { display?: string }).display || m.id}</SelectItem>
                ))
            }
          </Select>
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
