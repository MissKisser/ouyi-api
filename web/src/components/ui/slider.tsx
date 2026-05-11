"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

const Slider = React.forwardRef<
  HTMLInputElement,
  Omit<React.ComponentProps<"input">, "type" | "value" | "onChange" | "min" | "max" | "step"> & {
    min?: number
    max?: number
    step?: number
    value?: number[]
    onValueChange?: (value: number[]) => void
  }
>(({ className, min = 0, max = 100, step = 1, value = [0], onValueChange, ...props }, ref) => {
  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = parseFloat(e.target.value)
    onValueChange?.([val])
  }

  const valueStr = String(value[0] ?? min)

  return (
    <input
      ref={ref}
      type="range"
      min={min}
      max={max}
      step={step}
      value={valueStr}
      onChange={handleChange}
      data-slot="slider"
      className={cn(
        "w-full h-2 rounded-full cursor-pointer accent-primary bg-secondary",
        className
      )}
      {...props}
    />
  )
})
Slider.displayName = "Slider"

export { Slider }
