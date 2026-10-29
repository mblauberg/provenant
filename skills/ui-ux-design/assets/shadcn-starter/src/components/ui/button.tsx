import { Button as Primitive } from "@base-ui/react/button"
import { cn } from "@/lib/utils"

export function Button({ className, ...props }: Primitive.Props) {
  return <Primitive {...props} className={(state) => cn("rounded bg-primary px-4 py-2 text-primary-foreground focus-visible:outline-2", typeof className === "function" ? className(state) : className)} />
}
