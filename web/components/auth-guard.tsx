"use client"

import { useEffect, useState } from "react"
import { useRouter } from "next/navigation"
import { isAuthenticated, clearAuthToken, api } from "@/lib/api"

export function AuthGuard({ children }: { children: React.ReactNode }) {
  const router = useRouter()
  const [checked, setChecked] = useState(false)

  useEffect(() => {
    if (!isAuthenticated()) {
      router.push("/login")
      return
    }
    api.dashboard()
      .then(() => setChecked(true))
      .catch(() => {
        clearAuthToken()
        router.push("/login")
      })
  }, [router])

  if (!checked) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <p className="text-muted-foreground">Loading...</p>
      </div>
    )
  }

  return <>{children}</>
}
