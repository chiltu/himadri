"use client"

import { useState } from "react"
import { useRouter } from "next/navigation"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import {
  Field,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { api, setAuthToken } from "@/lib/api"

export function LoginForm({
  className,
  ...props
}: React.ComponentProps<"div">) {
  const [key, setKey] = useState("")
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const router = useRouter()

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setLoading(true)

    try {
      const valid = await api.testAuth(key.trim())
      if (valid) {
        setAuthToken(key.trim())
        router.push("/dashboard")
      } else {
        setError("Invalid key")
      }
    } catch {
      setError("Invalid key or server unreachable")
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className={cn("flex flex-col gap-6", className)} {...props}>
      <Card className="overflow-hidden p-0">
        <CardContent className="p-6 md:p-8">
          <form onSubmit={handleSubmit}>
            <FieldGroup>
              <div className="flex flex-col items-center gap-2 text-center">
                <h1 className="text-2xl font-bold">Welcome back</h1>
                <p className="text-balance text-muted-foreground">
                  Enter your master key to access the gateway
                </p>
              </div>
              <Field>
                <FieldLabel htmlFor="key">Master Key</FieldLabel>
                <Input
                  id="key"
                  type="password"
                  value={key}
                  onChange={(e) => setKey(e.target.value)}
                  placeholder="Enter your master key"
                  autoFocus
                  required
                />
              </Field>
              {error && (
                <p className="text-sm text-destructive text-center">{error}</p>
              )}
              <Field>
                <Button type="submit" className="w-full" disabled={loading}>
                  {loading ? "Verifying..." : "Sign In"}
                </Button>
              </Field>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
