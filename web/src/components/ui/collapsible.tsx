"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

/** 折叠容器，通过 open / onOpenChange 受控展开状态。 */
const Collapsible = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    open?: boolean
    onOpenChange?: (open: boolean) => void
  }
>(({ className, children, ...props }, ref) => {
  return (
    <div ref={ref} data-slot="collapsible" className={cn("", className)} {...props}>
      {children}
    </div>
  )
})
Collapsible.displayName = "Collapsible"

/** 折叠触发按钮（透传 onClick，展开逻辑由调用方控制）。 */
const CollapsibleTrigger = React.forwardRef<
  HTMLButtonElement,
  React.ComponentProps<"button"> & {
    onClick?: (e: React.MouseEvent<HTMLButtonElement>) => void
  }
>(({ className, onClick, ...props }, ref) => {
  return (
    <button
      ref={ref}
      data-slot="collapsible-trigger"
      className={cn("", className)}
      onClick={onClick}
      {...props}
    />
  )
})
CollapsibleTrigger.displayName = "CollapsibleTrigger"

/** 折叠内容区，open 为 false 时直接不渲染子内容。 */
const CollapsibleContent = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    open?: boolean
  }
>(({ className, open = false, children, ...props }, ref) => {
  if (!open) return null
  return (
    <div ref={ref} data-slot="collapsible-content" className={cn("", className)} {...props}>
      {children}
    </div>
  )
})
CollapsibleContent.displayName = "CollapsibleContent"

export { Collapsible, CollapsibleTrigger, CollapsibleContent }
