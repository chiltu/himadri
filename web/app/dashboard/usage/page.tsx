"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState } from "react"
import { api, type UsageStats, type ApiKey } from "@/lib/api"
import { AppSidebar } from "@/components/app-sidebar"
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb"
import { Separator } from "@/components/ui/separator"
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

const ALL_KEYS = "__all__"

export default function UsagePage() {
  const [stats, setStats] = useState<UsageStats | null>(null)
  const [keys, setKeys] = useState<ApiKey[]>([])
  const [keyId, setKeyId] = useState(ALL_KEYS)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api.usageStats().then(setStats).catch((e) => setError(e.message))
    api.listKeys().then(setKeys).catch((e) => setError(e.message))
  }, [])

  const handleFilterChange = async (value: string) => {
    setKeyId(value)
    try {
      const data = value === ALL_KEYS ? await api.usageStats() : await api.keyUsageStats(value)
      setStats(data)
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to load usage stats")
    }
  }

  return (
    <AuthGuard>
    <SidebarProvider>
      <AppSidebar />
      <SidebarInset>
        <header className="flex h-16 shrink-0 items-center gap-2 transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12">
          <div className="flex items-center gap-2 px-4">
            <SidebarTrigger className="-ml-1" />
            <Separator orientation="vertical" className="mr-2 data-vertical:h-4 data-vertical:self-auto" />
            <Breadcrumb>
              <BreadcrumbList>
                <BreadcrumbItem className="hidden md:block">
                  <BreadcrumbLink href="#">API Gateway</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="hidden md:block" />
                <BreadcrumbItem>
                  <BreadcrumbPage>Usage</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </header>
        <div className="flex flex-1 flex-col gap-4 p-4 pt-0">
          {error && (
            <div className="rounded-md bg-destructive/10 p-4 text-sm text-destructive">
              {error}
              <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
            </div>
          )}

          <div className="flex items-center justify-between gap-4">
            <h1 className="text-lg font-semibold">Usage</h1>
            <Select value={keyId} onValueChange={handleFilterChange}>
              <SelectTrigger className="w-[240px]">
                <SelectValue placeholder="Filter by API key" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_KEYS}>All API keys</SelectItem>
                {keys.map((k) => (
                  <SelectItem key={k.id} value={k.id}>{k.name}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {stats && (
            <div className="grid auto-rows-min gap-4 md:grid-cols-4">
              <Card>
                <CardHeader className="pb-2">
                  <CardTitle className="text-sm font-medium text-muted-foreground">Total Requests</CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">{stats.total_requests.toLocaleString()}</div>
                </CardContent>
              </Card>
              <Card>
                <CardHeader className="pb-2">
                  <CardTitle className="text-sm font-medium text-muted-foreground">Total Tokens</CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">{stats.total_tokens.toLocaleString()}</div>
                </CardContent>
              </Card>
              <Card>
                <CardHeader className="pb-2">
                  <CardTitle className="text-sm font-medium text-muted-foreground">Total Cost</CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">${stats.total_cost_usd.toFixed(4)}</div>
                </CardContent>
              </Card>
              <Card>
                <CardHeader className="pb-2">
                  <CardTitle className="text-sm font-medium text-muted-foreground">Avg Latency</CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">{stats.avg_latency_ms.toFixed(0)}ms</div>
                </CardContent>
              </Card>
            </div>
          )}
        </div>
      </SidebarInset>
    </SidebarProvider>
    </AuthGuard>
  )
}
