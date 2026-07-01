"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState, useCallback } from "react"
import {
  api,
  type GatewayConfig,
  type Target,
  type PluginConfig,
  type RolePolicy,
  type ConfigHistoryEntry,
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
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"

const STRATEGY_MODES = [
  "single",
  "fallback",
  "load_balance",
  "least_latency",
  "cost_optimized",
  "conditional",
  "content_based",
  "ab_test",
] as const

function csv(v?: string[] | null): string {
  return v?.join(", ") ?? ""
}

function fromCsv(v: string): string[] | undefined {
  const items = v.split(",").map((s) => s.trim()).filter(Boolean)
  return items.length ? items : undefined
}

export default function ConfigPage() {
  const [config, setConfig] = useState<GatewayConfig | null>(null)
  const [orgsJson, setOrgsJson] = useState("")
  const [strategyRulesJson, setStrategyRulesJson] = useState("")
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [history, setHistory] = useState<ConfigHistoryEntry[]>([])
  const [saving, setSaving] = useState(false)

  const loadConfig = useCallback(() => {
    api.getConfig().then((c) => {
      setConfig(c)
      setOrgsJson(JSON.stringify(c.orgs, null, 2))
      setStrategyRulesJson(
        JSON.stringify(
          {
            conditional_rules: c.strategy.conditional_rules,
            content_rules: c.strategy.content_rules,
            ab_variants: c.strategy.ab_variants,
            strategy_fallback: c.strategy.strategy_fallback ?? null,
          },
          null,
          2
        )
      )
    }).catch((e) => setError(e.message))
  }, [])

  const loadHistory = useCallback(() => {
    api.configHistory().then((r) => setHistory(r.data)).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    loadConfig()
    loadHistory()
  }, [loadConfig, loadHistory])

  const update = (patch: (c: GatewayConfig) => GatewayConfig) => {
    setConfig((c) => (c ? patch(structuredClone(c)) : c))
  }

  const handleSave = async () => {
    if (!config) return
    setSaving(true)
    setError(null)
    try {
      let orgs: GatewayConfig["orgs"]
      let rules: {
        conditional_rules: GatewayConfig["strategy"]["conditional_rules"]
        content_rules: GatewayConfig["strategy"]["content_rules"]
        ab_variants: GatewayConfig["strategy"]["ab_variants"]
        strategy_fallback: Target | null
      }
      try {
        orgs = JSON.parse(orgsJson)
      } catch {
        throw new Error("Orgs & Teams JSON is invalid")
      }
      try {
        rules = JSON.parse(strategyRulesJson)
      } catch {
        throw new Error("Advanced strategy rules JSON is invalid")
      }
      const next: GatewayConfig = {
        ...config,
        orgs,
        strategy: {
          ...config.strategy,
          conditional_rules: rules.conditional_rules ?? [],
          content_rules: rules.content_rules ?? [],
          ab_variants: rules.ab_variants ?? [],
          strategy_fallback: rules.strategy_fallback ?? undefined,
        },
      }
      await api.updateConfig(next)
      setNotice("Config saved and hot-reloaded.")
      loadConfig()
      loadHistory()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to save config")
    } finally {
      setSaving(false)
    }
  }

  const handleReload = async () => {
    try {
      await api.reloadConfig()
      setNotice("Config reloaded from environment.")
      loadConfig()
      loadHistory()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to reload config")
    }
  }

  const handleRollback = async (version: number) => {
    if (!confirm(`Roll back to version ${version}? This takes effect immediately.`)) return
    try {
      await api.rollbackConfig(version)
      setNotice(`Rolled back to version ${version}.`)
      loadConfig()
      loadHistory()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to rollback config")
    }
  }

  if (!config) {
    return (
      <AuthGuard>
        <SidebarProvider>
          <AppSidebar />
          <SidebarInset>
            <div className="p-4 text-muted-foreground text-sm">Loading config…</div>
          </SidebarInset>
        </SidebarProvider>
      </AuthGuard>
    )
  }

  const roleEntries = Object.entries(config.rbac.roles)

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
                    <BreadcrumbLink href="#">Settings</BreadcrumbLink>
                  </BreadcrumbItem>
                  <BreadcrumbSeparator className="hidden md:block" />
                  <BreadcrumbItem>
                    <BreadcrumbPage>Config</BreadcrumbPage>
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
            {notice && (
              <div className="rounded-md bg-green-500/10 p-3 text-sm text-green-600">
                {notice}
                <Button variant="ghost" size="sm" className="ml-2" onClick={() => setNotice(null)}>Dismiss</Button>
              </div>
            )}

            <div className="flex items-center justify-end gap-2">
              <Button variant="outline" onClick={handleReload}>Reload from Env</Button>
              <Button onClick={handleSave} disabled={saving}>{saving ? "Saving…" : "Save Config"}</Button>
            </div>

            <Tabs defaultValue="general">
              <TabsList>
                <TabsTrigger value="general">General</TabsTrigger>
                <TabsTrigger value="targets">Targets</TabsTrigger>
                <TabsTrigger value="rbac">RBAC</TabsTrigger>
                <TabsTrigger value="plugins">Plugins</TabsTrigger>
                <TabsTrigger value="advanced">Advanced</TabsTrigger>
                <TabsTrigger value="history">History</TabsTrigger>
              </TabsList>

              <TabsContent value="general" className="space-y-4">
                <Card>
                  <CardHeader><CardTitle>Routing Strategy</CardTitle></CardHeader>
                  <CardContent className="grid grid-cols-2 gap-4">
                    <div>
                      <Label>Mode</Label>
                      <select
                        value={config.strategy.mode}
                        onChange={(e) => update((c) => { c.strategy.mode = e.target.value as GatewayConfig["strategy"]["mode"]; return c })}
                        className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors"
                      >
                        {STRATEGY_MODES.map((m) => <option key={m} value={m}>{m}</option>)}
                      </select>
                    </div>
                    <div>
                      <Label>Fallback timeout (ms)</Label>
                      <Input
                        type="number"
                        value={config.strategy.fallback_timeout_ms}
                        onChange={(e) => update((c) => { c.strategy.fallback_timeout_ms = Number(e.target.value) || 0; return c })}
                      />
                    </div>
                  </CardContent>
                </Card>

                <Card>
                  <CardHeader><CardTitle>Rate Limit</CardTitle></CardHeader>
                  <CardContent className="grid grid-cols-3 gap-4">
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.rate_limit.enabled}
                        onChange={(e) => update((c) => { c.rate_limit.enabled = e.target.checked; return c })}
                      />
                      Enabled
                    </label>
                    <div>
                      <Label>Requests/sec</Label>
                      <Input
                        type="number"
                        value={config.rate_limit.requests_per_second}
                        onChange={(e) => update((c) => { c.rate_limit.requests_per_second = Number(e.target.value) || 0; return c })}
                      />
                    </div>
                    <div>
                      <Label>Burst size</Label>
                      <Input
                        type="number"
                        value={config.rate_limit.burst_size}
                        onChange={(e) => update((c) => { c.rate_limit.burst_size = Number(e.target.value) || 0; return c })}
                      />
                    </div>
                  </CardContent>
                </Card>

                <Card>
                  <CardHeader><CardTitle>CORS</CardTitle></CardHeader>
                  <CardContent className="space-y-4">
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.cors.enabled}
                        onChange={(e) => update((c) => { c.cors.enabled = e.target.checked; return c })}
                      />
                      Enabled
                    </label>
                    <div>
                      <Label>Allowed origins (comma-separated, empty = none)</Label>
                      <Input
                        value={csv(config.cors.allowed_origins)}
                        onChange={(e) => update((c) => { c.cors.allowed_origins = fromCsv(e.target.value) ?? []; return c })}
                      />
                    </div>
                    <div>
                      <Label>Allowed methods</Label>
                      <Input
                        value={csv(config.cors.allowed_methods)}
                        onChange={(e) => update((c) => { c.cors.allowed_methods = fromCsv(e.target.value) ?? []; return c })}
                      />
                    </div>
                    <div>
                      <Label>Allowed headers</Label>
                      <Input
                        value={csv(config.cors.allowed_headers)}
                        onChange={(e) => update((c) => { c.cors.allowed_headers = fromCsv(e.target.value) ?? []; return c })}
                      />
                    </div>
                  </CardContent>
                </Card>

                <Card>
                  <CardHeader><CardTitle>Admin &amp; Observability</CardTitle></CardHeader>
                  <CardContent className="grid grid-cols-2 gap-4">
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.admin.enabled}
                        onChange={(e) => update((c) => { c.admin.enabled = e.target.checked; return c })}
                      />
                      Admin API enabled
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.observability.tracing.enabled}
                        onChange={(e) => update((c) => { c.observability.tracing.enabled = e.target.checked; return c })}
                      />
                      OTel tracing enabled
                    </label>
                    <div>
                      <Label>Tracing endpoint</Label>
                      <Input
                        value={config.observability.tracing.endpoint ?? ""}
                        onChange={(e) => update((c) => { c.observability.tracing.endpoint = e.target.value || undefined; return c })}
                      />
                    </div>
                    <div>
                      <Label>Trace sample ratio</Label>
                      <Input
                        type="number"
                        step="0.1"
                        value={config.observability.tracing.sample_ratio}
                        onChange={(e) => update((c) => { c.observability.tracing.sample_ratio = Number(e.target.value) || 0; return c })}
                      />
                    </div>
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="targets">
                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <CardTitle>Routing Targets</CardTitle>
                      <Button
                        size="sm"
                        onClick={() => update((c) => {
                          c.targets.push({ provider: "", weight: 1, models: undefined, api_key_env: undefined, base_url: undefined })
                          return c
                        })}
                      >
                        Add Target
                      </Button>
                    </div>
                  </CardHeader>
                  <CardContent>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Provider</TableHead>
                          <TableHead>Weight</TableHead>
                          <TableHead>Models (CSV)</TableHead>
                          <TableHead>API Key Env</TableHead>
                          <TableHead>Base URL</TableHead>
                          <TableHead className="text-right">Actions</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {config.targets.map((t, i) => (
                          <TableRow key={i}>
                            <TableCell>
                              <Input value={t.provider} onChange={(e) => update((c) => { c.targets[i].provider = e.target.value; return c })} />
                            </TableCell>
                            <TableCell>
                              <Input type="number" value={t.weight} onChange={(e) => update((c) => { c.targets[i].weight = Number(e.target.value) || 0; return c })} />
                            </TableCell>
                            <TableCell>
                              <Input value={csv(t.models)} onChange={(e) => update((c) => { c.targets[i].models = fromCsv(e.target.value); return c })} />
                            </TableCell>
                            <TableCell>
                              <Input value={t.api_key_env ?? ""} onChange={(e) => update((c) => { c.targets[i].api_key_env = e.target.value || undefined; return c })} />
                            </TableCell>
                            <TableCell>
                              <Input value={t.base_url ?? ""} onChange={(e) => update((c) => { c.targets[i].base_url = e.target.value || undefined; return c })} />
                            </TableCell>
                            <TableCell className="text-right">
                              <Button
                                variant="ghost"
                                size="sm"
                                className="text-destructive"
                                onClick={() => update((c) => { c.targets.splice(i, 1); return c })}
                              >
                                Remove
                              </Button>
                            </TableCell>
                          </TableRow>
                        ))}
                        {config.targets.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={6} className="text-center text-muted-foreground py-8">No targets configured</TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="rbac" className="space-y-4">
                <Card>
                  <CardHeader><CardTitle>RBAC</CardTitle></CardHeader>
                  <CardContent className="space-y-4">
                    <div className="flex items-center gap-4">
                      <label className="flex items-center gap-2 text-sm">
                        <input
                          type="checkbox"
                          checked={config.rbac.enabled}
                          onChange={(e) => update((c) => { c.rbac.enabled = e.target.checked; return c })}
                        />
                        Enabled
                      </label>
                      <div className="flex-1">
                        <Label>Default role</Label>
                        <Input
                          value={config.rbac.default_role ?? ""}
                          onChange={(e) => update((c) => { c.rbac.default_role = e.target.value || undefined; return c })}
                          placeholder="e.g. free"
                        />
                      </div>
                    </div>
                  </CardContent>
                </Card>

                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <CardTitle>Role Policies</CardTitle>
                      <Button
                        size="sm"
                        onClick={() => update((c) => {
                          let name = "new_role"
                          let n = 1
                          while (c.rbac.roles[name]) name = `new_role_${n++}`
                          c.rbac.roles[name] = { models: undefined, providers: undefined }
                          return c
                        })}
                      >
                        Add Role
                      </Button>
                    </div>
                  </CardHeader>
                  <CardContent>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Role</TableHead>
                          <TableHead>Allowed models (CSV, * for all)</TableHead>
                          <TableHead>Allowed providers (CSV, * for all)</TableHead>
                          <TableHead className="text-right">Actions</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {roleEntries.map(([name, policy]: [string, RolePolicy]) => (
                          <TableRow key={name}>
                            <TableCell>
                              <Input
                                value={name}
                                onChange={(e) => update((c) => {
                                  const renamed = e.target.value
                                  const roles: Record<string, RolePolicy> = {}
                                  for (const [k, v] of Object.entries(c.rbac.roles)) {
                                    roles[k === name ? renamed : k] = v
                                  }
                                  c.rbac.roles = roles
                                  return c
                                })}
                              />
                            </TableCell>
                            <TableCell>
                              <Input
                                value={csv(policy.models)}
                                onChange={(e) => update((c) => { c.rbac.roles[name].models = fromCsv(e.target.value); return c })}
                              />
                            </TableCell>
                            <TableCell>
                              <Input
                                value={csv(policy.providers)}
                                onChange={(e) => update((c) => { c.rbac.roles[name].providers = fromCsv(e.target.value); return c })}
                              />
                            </TableCell>
                            <TableCell className="text-right">
                              <Button
                                variant="ghost"
                                size="sm"
                                className="text-destructive"
                                onClick={() => update((c) => { delete c.rbac.roles[name]; return c })}
                              >
                                Remove
                              </Button>
                            </TableCell>
                          </TableRow>
                        ))}
                        {roleEntries.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={4} className="text-center text-muted-foreground py-8">No roles configured</TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="plugins">
                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <CardTitle>Plugins</CardTitle>
                      <Button
                        size="sm"
                        onClick={() => update((c) => {
                          c.plugins.push({ name: "", enabled: false, config: {} } as PluginConfig)
                          return c
                        })}
                      >
                        Add Plugin
                      </Button>
                    </div>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    {config.plugins.map((p, i) => (
                      <div key={i} className="rounded-md border p-3 space-y-2">
                        <div className="flex items-center gap-4">
                          <Input
                            className="flex-1"
                            value={p.name}
                            onChange={(e) => update((c) => { c.plugins[i].name = e.target.value; return c })}
                            placeholder="plugin name"
                          />
                          <label className="flex items-center gap-2 text-sm whitespace-nowrap">
                            <input
                              type="checkbox"
                              checked={p.enabled}
                              onChange={(e) => update((c) => { c.plugins[i].enabled = e.target.checked; return c })}
                            />
                            Enabled
                          </label>
                          <Badge variant="secondary">{p.enabled ? "on" : "off"}</Badge>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive"
                            onClick={() => update((c) => { c.plugins.splice(i, 1); return c })}
                          >
                            Remove
                          </Button>
                        </div>
                        <div>
                          <Label>Config (JSON)</Label>
                          <Textarea
                            value={JSON.stringify(p.config ?? {}, null, 2)}
                            onChange={(e) => {
                              try {
                                const parsed = JSON.parse(e.target.value)
                                update((c) => { c.plugins[i].config = parsed; return c })
                              } catch {
                                // ignore invalid JSON while typing
                              }
                            }}
                          />
                        </div>
                      </div>
                    ))}
                    {config.plugins.length === 0 && (
                      <p className="text-center text-muted-foreground py-8 text-sm">No plugins configured</p>
                    )}
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="advanced" className="space-y-4">
                <Card>
                  <CardHeader><CardTitle>Orgs &amp; Teams (JSON)</CardTitle></CardHeader>
                  <CardContent>
                    <p className="text-sm text-muted-foreground mb-2">
                      Per-org / per-team allow-lists, budgets, rate limits, guardrails and audit config. Edit as raw JSON — validated on save.
                    </p>
                    <Textarea className="min-h-64 font-mono" value={orgsJson} onChange={(e) => setOrgsJson(e.target.value)} />
                  </CardContent>
                </Card>
                <Card>
                  <CardHeader><CardTitle>Advanced Strategy Rules (JSON)</CardTitle></CardHeader>
                  <CardContent>
                    <p className="text-sm text-muted-foreground mb-2">
                      conditional_rules / content_rules / ab_variants / strategy_fallback — only used when strategy mode is conditional, content_based or ab_test.
                    </p>
                    <Textarea className="min-h-64 font-mono" value={strategyRulesJson} onChange={(e) => setStrategyRulesJson(e.target.value)} />
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="history">
                <Card>
                  <CardHeader><CardTitle>Config History</CardTitle></CardHeader>
                  <CardContent>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Version</TableHead>
                          <TableHead>Updated</TableHead>
                          <TableHead>Rolled back from</TableHead>
                          <TableHead className="text-right">Actions</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {history.map((h) => (
                          <TableRow key={h.version}>
                            <TableCell>{h.version}</TableCell>
                            <TableCell className="text-sm">{new Date(h.updated_at).toLocaleString()}</TableCell>
                            <TableCell>{h.rolled_back_from ?? "-"}</TableCell>
                            <TableCell className="text-right">
                              <Button variant="outline" size="sm" onClick={() => handleRollback(h.version)}>
                                Rollback
                              </Button>
                            </TableCell>
                          </TableRow>
                        ))}
                        {history.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={4} className="text-center text-muted-foreground py-8">No history yet</TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </CardContent>
                </Card>
              </TabsContent>
            </Tabs>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </AuthGuard>
  )
}
