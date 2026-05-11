"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

const Collapsible = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    open?: boolean
    onOpenChange?: (open: boolean) => void
  }
>(({ className, open = false, onOpenChange, children, ...props }, ref) => {
  return (
    <div ref={ref} data-slot="collapsible" className={cn("", className)} {...props}>
      {children}
    </div>
  )
})
Collapsible.displayName = "Collapsible"

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
