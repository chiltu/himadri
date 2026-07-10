"use client"

import { useEffect, useState } from "react"
import { api, type DashboardSummary } from "@/lib/api"
import { AuthGuard } from "@/components/auth-guard"
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Badge } from "@/components/ui/badge"

export default function DashboardPage() {
  const [data, setData] = useState<DashboardSummary | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api.dashboard().then(setData).catch((e) => setError(e.message))
  }, [])

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
                  <BreadcrumbLink href="#">Platform</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="hidden md:block" />
                <BreadcrumbItem>
                  <BreadcrumbPage>Dashboard</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </header>
        <div className="flex flex-1 flex-col gap-4 p-4 pt-0">
          {error && (
            <div className="rounded-md bg-destructive/10 p-4 text-sm text-destructive">
              Failed to load dashboard: {error}
            </div>
          )}

          {data && (
            <>
              <div className="grid auto-rows-min gap-6 border-b pb-6 sm:grid-cols-2 md:grid-cols-4">
                <div>
                  <div className="text-sm text-muted-foreground">Total Requests</div>
                  <div className="mt-1 text-2xl font-bold">{data.total_requests.toLocaleString()}</div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">Total Tokens</div>
                  <div className="mt-1 text-2xl font-bold">{data.total_tokens.toLocaleString()}</div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">Total Cost</div>
                  <div className="mt-1 text-2xl font-bold">${data.total_cost_usd.toFixed(4)}</div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">Avg Latency</div>
                  <div className="mt-1 text-2xl font-bold">{data.avg_latency_ms.toFixed(0)}ms</div>
                </div>
              </div>

              <div className="grid gap-8 md:grid-cols-2">
                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Top Models</h2>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Model</TableHead>
                        <TableHead className="text-right">Requests</TableHead>
                        <TableHead className="text-right">Cost</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {data.top_models.map((m) => (
                        <TableRow key={m.model}>
                          <TableCell className="font-mono text-sm">{m.model}</TableCell>
                          <TableCell className="text-right">{m.requests.toLocaleString()}</TableCell>
                          <TableCell className="text-right">${m.cost_usd.toFixed(4)}</TableCell>
                        </TableRow>
                      ))}
                      {data.top_models.length === 0 && (
                        <TableRow>
                          <TableCell colSpan={3} className="text-center text-muted-foreground">No data yet</TableCell>
                        </TableRow>
                      )}
                    </TableBody>
                  </Table>
                </section>

                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Top Providers</h2>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Provider</TableHead>
                        <TableHead className="text-right">Requests</TableHead>
                        <TableHead className="text-right">Cost</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {data.top_providers.map((p) => (
                        <TableRow key={p.provider}>
                          <TableCell className="font-mono text-sm">{p.provider}</TableCell>
                          <TableCell className="text-right">{p.requests.toLocaleString()}</TableCell>
                          <TableCell className="text-right">${p.cost_usd.toFixed(4)}</TableCell>
                        </TableRow>
                      ))}
                      {data.top_providers.length === 0 && (
                        <TableRow>
                          <TableCell colSpan={3} className="text-center text-muted-foreground">No data yet</TableCell>
                        </TableRow>
                      )}
                    </TableBody>
                  </Table>
                </section>
              </div>

              {data.recent_errors.length > 0 && (
                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Recent Errors</h2>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Time</TableHead>
                        <TableHead>Model</TableHead>
                        <TableHead>Error</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {data.recent_errors.map((e) => (
                        <TableRow key={e.request_id}>
                          <TableCell className="text-sm">{new Date(e.created_at).toLocaleString()}</TableCell>
                          <TableCell className="font-mono text-sm">{e.model}</TableCell>
                          <TableCell>
                            <Badge variant="destructive">{e.error_message || "Unknown error"}</Badge>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </section>
              )}
            </>
          )}
        </div>
      </SidebarInset>
    </SidebarProvider>
    </AuthGuard>
  )
}
