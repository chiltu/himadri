"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState, useCallback } from "react"
import {
  api,
  DEFAULT_PII_GUARDRAIL_CONFIG,
  type GatewayConfig,
  type Target,
  type RolePolicy,
  type ConfigHistoryEntry,
  type PiiMode,
  type PiiStrategy,
  type PiiResponseMode,
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
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

const PII_MODES: PiiMode[] = ["redact", "block", "observe"]
const PII_STRATEGIES: PiiStrategy[] = ["replace", "mask", "hash", "encrypt", "remove"]
const PII_RESPONSE_MODES: PiiResponseMode[] = ["off", "observe", "redact", "block"]

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
      // Older gateways may not serve the guardrails section yet.
      c.guardrails ??= { pii: { ...DEFAULT_PII_GUARDRAIL_CONFIG } }
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
                <TabsTrigger value="guardrails">Guardrails</TabsTrigger>
                <TabsTrigger value="rbac">RBAC</TabsTrigger>
                <TabsTrigger value="advanced">Advanced</TabsTrigger>
                <TabsTrigger value="history">History</TabsTrigger>
              </TabsList>

              <TabsContent value="general" className="space-y-8">
                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Routing Strategy</h2>
                  <p className="text-sm text-muted-foreground">
                    Routing targets (model × provider endpoint) are managed on the{" "}
                    <a href="/dashboard/models" className="underline">Models</a> page.
                  </p>
                  <div className="grid grid-cols-2 gap-4">
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
                  </div>
                </section>

                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Rate Limit</h2>
                  <div className="grid grid-cols-3 gap-4">
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
                  </div>
                </section>

                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">CORS</h2>
                  <p className="text-sm text-amber-600">
                    Applied at startup — saving persists these values, but a gateway restart is required for them to take effect.
                  </p>
                  <div className="space-y-4">
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
                  </div>
                </section>

                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">Observability</h2>
                  <p className="text-sm text-amber-600">
                    Applied at startup — saving persists these values, but a gateway restart is required for them to take effect.
                  </p>
                  <div className="grid grid-cols-2 gap-4">
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
                  </div>
                </section>
              </TabsContent>

              <TabsContent value="guardrails" className="space-y-8">
                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">PII Guardrail (global default)</h2>
                  <p className="text-sm text-muted-foreground">
                    Scans request content before it is forwarded to any LLM provider.
                    Applies hot on save. Per-org/team overrides live in the{" "}
                    <code>guardrails.pii</code> section of Orgs &amp; Teams (Advanced tab)
                    and replace these settings wholesale for that scope.
                  </p>
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={config.guardrails.pii.enabled}
                      onChange={(e) => update((c) => { c.guardrails.pii.enabled = e.target.checked; return c })}
                    />
                    Enabled
                  </label>
                  <div className="grid grid-cols-3 gap-4">
                    <div>
                      <Label>Mode</Label>
                      <select
                        value={config.guardrails.pii.mode}
                        onChange={(e) => update((c) => { c.guardrails.pii.mode = e.target.value as PiiMode; return c })}
                        className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors"
                      >
                        {PII_MODES.map((m) => <option key={m} value={m}>{m}</option>)}
                      </select>
                      <p className="mt-1 text-xs text-muted-foreground">
                        redact rewrites spans; block rejects with 400; observe only records metrics.
                      </p>
                    </div>
                    <div>
                      <Label>Strategy</Label>
                      <select
                        value={config.guardrails.pii.strategy}
                        onChange={(e) => update((c) => { c.guardrails.pii.strategy = e.target.value as PiiStrategy; return c })}
                        className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors"
                      >
                        {PII_STRATEGIES.map((s) => <option key={s} value={s}>{s}</option>)}
                      </select>
                      <p className="mt-1 text-xs text-muted-foreground">
                        hash/encrypt need GUARDRAILS_HASH_SALT / GUARDRAILS_ENCRYPTION_KEY set on the gateway.
                      </p>
                    </div>
                    <div>
                      <Label>Min confidence (0–1)</Label>
                      <Input
                        type="number"
                        step="0.05"
                        min="0"
                        max="1"
                        value={config.guardrails.pii.min_confidence}
                        onChange={(e) => update((c) => { c.guardrails.pii.min_confidence = Number(e.target.value) || 0; return c })}
                      />
                    </div>
                    <div>
                      <Label>Response mode</Label>
                      <select
                        value={config.guardrails.pii.response_mode}
                        onChange={(e) => update((c) => { c.guardrails.pii.response_mode = e.target.value as PiiResponseMode; return c })}
                        className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors"
                      >
                        {PII_RESPONSE_MODES.map((m) => <option key={m} value={m}>{m}</option>)}
                      </select>
                      <p className="mt-1 text-xs text-muted-foreground">
                        Scans model output. Non-streaming only; streams are checked post-hoc at stream end.
                      </p>
                    </div>
                  </div>
                  <div className="grid grid-cols-2 gap-4">
                    <div>
                      <Label>Entity types (CSV, empty = all)</Label>
                      <Input
                        placeholder="EMAIL_ADDRESS, US_SSN, CREDIT_CARD"
                        value={csv(config.guardrails.pii.entities)}
                        onChange={(e) => update((c) => { c.guardrails.pii.entities = fromCsv(e.target.value) ?? null; return c })}
                      />
                    </div>
                    <div>
                      <Label>Scanned roles (CSV)</Label>
                      <Input
                        placeholder="user, system, tool"
                        value={csv(config.guardrails.pii.apply_to)}
                        onChange={(e) => update((c) => { c.guardrails.pii.apply_to = fromCsv(e.target.value) ?? []; return c })}
                      />
                    </div>
                  </div>
                  <div className="flex items-center gap-6">
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.guardrails.pii.scan_tool_arguments}
                        onChange={(e) => update((c) => { c.guardrails.pii.scan_tool_arguments = e.target.checked; return c })}
                      />
                      Scan tool-call arguments
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={config.guardrails.pii.fail_open}
                        onChange={(e) => update((c) => { c.guardrails.pii.fail_open = e.target.checked; return c })}
                      />
                      Fail open (forward unscanned on engine errors)
                    </label>
                  </div>
                </section>
              </TabsContent>

              <TabsContent value="rbac" className="space-y-8">
                <section className="space-y-3">
                  <h2 className="text-sm font-medium text-muted-foreground">RBAC</h2>
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
                </section>

                <section className="space-y-4">
                  <div className="flex items-center justify-between">
                    <h2 className="text-lg font-semibold">Role Policies</h2>
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
                </section>
              </TabsContent>

              <TabsContent value="advanced" className="space-y-8">
                <section className="space-y-2">
                  <h2 className="text-sm font-medium text-muted-foreground">Orgs &amp; Teams (JSON)</h2>
                  <p className="text-sm text-muted-foreground">
                    Per-org / per-team allow-lists, budgets, rate limits, guardrails and audit config. Edit as raw JSON — validated on save.
                  </p>
                  <Textarea className="min-h-64 font-mono" value={orgsJson} onChange={(e) => setOrgsJson(e.target.value)} />
                </section>
                <section className="space-y-2">
                  <h2 className="text-sm font-medium text-muted-foreground">Advanced Strategy Rules (JSON)</h2>
                  <p className="text-sm text-muted-foreground">
                    conditional_rules / content_rules / ab_variants / strategy_fallback — only used when strategy mode is conditional, content_based or ab_test.
                  </p>
                  <Textarea className="min-h-64 font-mono" value={strategyRulesJson} onChange={(e) => setStrategyRulesJson(e.target.value)} />
                </section>
              </TabsContent>

              <TabsContent value="history" className="space-y-4">
                <h2 className="text-lg font-semibold">Config History</h2>
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
              </TabsContent>
            </Tabs>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </AuthGuard>
  )
}
