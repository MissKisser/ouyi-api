import { Component, type ReactNode, type ErrorInfo } from 'react';

/** 边界组件的属性。 */
interface Props {
  /** 被边界包裹的子内容，正常时原样渲染。 */
  children: ReactNode;
}

/** 边界组件的内部状态。 */
interface State {
  /** 子树抛出错误后置为 true，后续渲染降级 UI。 */
  hasError: boolean;
  /** 捕获到的错误对象，无错误时为 null。 */
  error: Error | null;
}

/**
 * 全局错误边界：捕获子组件渲染期的未处理异常，降级为全屏错误页
 * （展示错误信息并提供"重新加载"按钮），同时把错误与组件栈打印到 console。
 * 恢复方式为整页刷新；事件处理器与异步回调中的异常不经过此边界。
 */
export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('[ErrorBoundary]', error, info.componentStack);
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="min-h-screen flex items-center justify-center bg-background p-4">
          <div className="text-center space-y-4 max-w-md">
            <div className="text-4xl">!</div>
            <h1 className="text-xl font-semibold">页面发生错误</h1>
            <p className="text-sm text-muted-foreground break-all">
              {this.state.error?.message || '未知错误'}
            </p>
            <button
              className="inline-flex items-center justify-center rounded-md text-sm font-medium border bg-background px-4 py-2 hover:bg-accent hover:text-accent-foreground transition-colors"
              onClick={() => window.location.reload()}
            >
              重新加载
            </button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
