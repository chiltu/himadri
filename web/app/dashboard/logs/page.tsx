"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState, useCallback } from "react"
import {
  api,
  type RequestLogEntry,
  type RequestLogListResult,
  type Model,
} from "@/lib/api"
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
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

const ALL = "__all__"

export default function LogsPage() {
  const [logs, setLogs] = useState<RequestLogEntry[]>([])
  const [total, setTotal] = useState(0)
  const [model, setModel] = useState("")
  const [provider, setProvider] = useState("")
  const [models, setModels] = useState<Model[]>([])
  const [providers, setProviders] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)

  const loadLogs = useCallback(() => {
    api.listLogs({ model: model || undefined, provider: provider || undefined })
      .then((r: RequestLogListResult) => { setLogs(r.data); setTotal(r.total) })
      .catch((e: Error) => setError(e.message))
  }, [model, provider])

  useEffect(() => { loadLogs() }, [loadLogs])

  useEffect(() => {
    api.listModels().then(setModels).catch((e) => setError(e.message))
    api
      .listAllEndpoints()
      .then((eps) => setProviders([...new Set(eps.map((e) => e.provider_type))]))
      .catch((e) => setError(e.message))
  }, [])

  const handleDeleteOld = async () => {
    const days = prompt("Delete logs older than how many days?", "30")
    if (!days) return
    try {
      const since = new Date(Date.now() - parseInt(days) * 86400000).toISOString()
      const result = await api.deleteLogs({ since })
      alert(`Deleted ${result.deleted} log entries`)
      loadLogs()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete logs")
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
                  <BreadcrumbLink href="#">Platform</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="hidden md:block" />
                <BreadcrumbItem>
                  <BreadcrumbPage>Request Logs</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </header>
        <div className="flex flex-1 flex-col gap-4 p-4 pt-0">
          {error && (
            <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
              {error}
              <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
            </div>
          )}

          <div className="flex flex-wrap items-center justify-between gap-3">
            <h1 className="text-lg font-semibold">Request Logs</h1>
            <div className="flex items-center gap-2">
              <Select value={model || ALL} onValueChange={(v) => setModel(v === ALL ? "" : v)}>
                <SelectTrigger className="w-[200px]">
                  <SelectValue placeholder="All models" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={ALL}>All models</SelectItem>
                  {models.map((m) => (
                    <SelectItem key={m.id} value={m.name}>{m.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Select value={provider || ALL} onValueChange={(v) => setProvider(v === ALL ? "" : v)}>
                <SelectTrigger className="w-[200px]">
                  <SelectValue placeholder="All providers" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={ALL}>All providers</SelectItem>
                  {providers.map((p) => (
                    <SelectItem key={p} value={p}>{p}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Button variant="destructive" onClick={handleDeleteOld}>Delete Old Logs</Button>
            </div>
          </div>

          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Trace ID</TableHead>
                <TableHead>Model</TableHead>
                <TableHead>Provider</TableHead>
                <TableHead className="text-right">Tokens</TableHead>
                <TableHead>Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {logs.map((log) => (
                <TableRow key={log.trace_id}>
                  <TableCell className="text-sm">{new Date(log.created_at).toLocaleString()}</TableCell>
                  <TableCell className="font-mono text-xs">{log.trace_id.slice(0, 8)}...</TableCell>
                  <TableCell className="font-mono text-sm">{log.model}</TableCell>
                  <TableCell>{log.provider}</TableCell>
                  <TableCell className="text-right">{log.total_tokens.toLocaleString()}</TableCell>
                  <TableCell>
                    {log.error_message ? (
                      <Badge variant="destructive">Error</Badge>
                    ) : (
                      <Badge variant="default">OK</Badge>
                    )}
                  </TableCell>
                </TableRow>
              ))}
              {logs.length === 0 && (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground py-8">No logs found</TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>

          <div className="text-sm text-muted-foreground">Total: {total} entries</div>
        </div>
      </SidebarInset>
    </SidebarProvider>
    </AuthGuard>
  )
}
