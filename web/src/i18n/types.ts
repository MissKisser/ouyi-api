import { resources } from './index';

/**
 * i18next 类型增强：把默认命名空间固定为 common，并基于 resources 的字面量类型
 * 为 t() 的翻译 key 提供编译期校验与自动补全。
 */
declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'common';
    resources: typeof resources;
  }
}
