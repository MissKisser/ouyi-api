"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

/** 原生 select 封装，用 value / onValueChange 受控，支持 placeholder 占位项。 */
const Select = React.forwardRef<
  HTMLSelectElement,
  React.ComponentProps<"select"> & {
    value?: string
    onValueChange?: (value: string) => void
    placeholder?: string
    children?: React.ReactNode
  }
>(({ className, value, onValueChange, placeholder, children, ...props }, ref) => {
  return (
    <select
      ref={ref}
      value={value}
      onChange={(e) => onValueChange?.(e.target.value)}
      data-slot="select"
      className={cn(
        "w-full rounded-lg border border-input bg-transparent px-3 py-2 text-sm transition-colors placeholder:text-muted-foreground focus-visible:border-ring focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    >
      {placeholder && (
        <option value="" disabled>
          {placeholder}
        </option>
      )}
      {children}
    </select>
  )
})
Select.displayName = "Select"

/** 下拉选项分组（optgroup 封装）。 */
const SelectGroup = React.forwardRef<
  HTMLOptGroupElement,
  React.ComponentProps<"optgroup">
>(({ className, ...props }, ref) => {
  return (
    <optgroup
      ref={ref}
      data-slot="select-group"
      className={cn("", className)}
      {...props}
    />
  )
})
SelectGroup.displayName = "SelectGroup"

/** 下拉单个选项（option 封装）。 */
const SelectItem = React.forwardRef<
  HTMLOptionElement,
  React.ComponentProps<"option">
>(({ className, ...props }, ref) => {
  return (
    <option
      ref={ref}
      data-slot="select-item"
      className={cn("", className)}
      {...props}
    />
  )
})
SelectItem.displayName = "SelectItem"

export { Select, SelectGroup, SelectItem }
