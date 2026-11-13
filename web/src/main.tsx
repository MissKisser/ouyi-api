import { StrictMode, Suspense } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { I18nextProvider } from 'react-i18next'
import i18n from './i18n'
import './index.css'
import App from './App.tsx'

/**
 * 应用入口：把根组件挂载到 #root，外层依次提供 StrictMode、i18n、路由 Suspense
 * 与 BrowserRouter（basename 取自部署 base 路径，根路径时不设置）。
 */
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <I18nextProvider i18n={i18n}>
      <Suspense fallback={<div>Loading...</div>}>
        <BrowserRouter basename={import.meta.env.BASE_URL.replace(/\/$/, '') || undefined}>
          <App />
        </BrowserRouter>
      </Suspense>
    </I18nextProvider>
  </StrictMode>,
)
