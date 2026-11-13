import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import LanguageDetector from 'i18next-browser-languagedetector';

import zhTranslation from '../locales/zh/common.json';
import enTranslation from '../locales/en/common.json';

/**
 * i18next 资源表：zh / en 两种语言各挂载一份 common 命名空间翻译（来自 locales 目录 JSON）。
 * 以 as const 导出，供 i18n/types.ts 推导翻译 key 的字面量类型。
 */
export const resources = {
  zh: {
    common: zhTranslation,
  },
  en: {
    common: enTranslation,
  },
} as const;

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources,
    fallbackLng: 'zh',
    debug: false,
    interpolation: {
      escapeValue: false,
    },
    defaultNS: 'common',
    detection: {
      order: ['localStorage', 'navigator'],
      caches: ['localStorage'],
    },
  });

/**
 * 全局 i18next 实例：使用浏览器语言检测（localStorage 优先，其次 navigator），
 * 中文为回退语言，默认命名空间为 common；语言偏好会缓存到 localStorage。
 * 由 main.tsx 通过 I18nextProvider 注入整个应用。
 */
export default i18n;
