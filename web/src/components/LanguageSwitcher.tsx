import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Languages } from 'lucide-react';

/**
 * 语言切换按钮（侧边栏底部使用）：点击在中/英文间切换 i18n 语言。
 * 副作用：调用 i18n.changeLanguage，语言偏好由 i18next 检测配置写入 localStorage；
 * 按钮文案始终显示"目标语言"（中文环境显示 English，英文环境显示 中文）。
 */
export function LanguageSwitcher() {
  const { i18n, t } = useTranslation();

  const toggleLanguage = () => {
    const nextLang = i18n.language === 'zh' ? 'en' : 'zh';
    i18n.changeLanguage(nextLang);
  };

  return (
    <Button
      variant="ghost"
      size="sm"
      onClick={toggleLanguage}
      className="w-full justify-start gap-3 text-muted-foreground"
    >
      <Languages className="h-4 w-4" />
      {i18n.language === 'zh' ? t('language.en') : t('language.zh')}
    </Button>
  );
}
